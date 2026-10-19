use super::{BackendTermLock, Processor};
use crate::{PtyEvent, TerminalBackendEvent, TerminalBounds, pty_info::ProcessIdGetter};
use alacritty_terminal::{
    event::{OnResize, WindowSize},
    tty::{self, EventedPty, EventedReadWrite},
};
use anyhow::{Context as _, Result};
use futures::channel::mpsc::UnboundedSender;
use polling::{Event as PollEvent, Events, PollMode, Poller};
use std::{
    borrow::Cow,
    collections::VecDeque,
    io::{self, Read, Write},
    path::PathBuf,
    sync::{Arc, mpsc},
};

enum Message {
    Input(Cow<'static, [u8]>),
    Resize(TerminalBounds),
    Shutdown,
}
pub(crate) struct PtySender {
    sender: mpsc::Sender<Message>,
    poller: Arc<Poller>,
}
impl PtySender {
    fn send(&self, message: Message) {
        if self.sender.send(message).is_err() {
            log::debug!("terminal PTY is closed");
            return;
        }
        if let Err(error) = self.poller.notify() {
            log::error!("failed to wake terminal PTY: {error}");
        }
    }
    pub(crate) fn notify(&self, input: impl Into<Cow<'static, [u8]>>) {
        self.send(Message::Input(input.into()));
    }
    pub(crate) fn resize(&self, bounds: TerminalBounds) {
        self.send(Message::Resize(bounds));
    }
    pub(crate) fn shutdown(&self) {
        self.send(Message::Shutdown);
    }
}
impl Drop for PtySender {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn window_size(bounds: TerminalBounds) -> WindowSize {
    WindowSize {
        num_lines: bounds.num_lines() as u16,
        num_cols: bounds.num_columns() as u16,
        cell_width: f32::from(bounds.cell_width()) as u16,
        cell_height: f32::from(bounds.line_height()) as u16,
    }
}
#[cfg(not(windows))]
pub(crate) fn current_child_signal_mask() -> io::Result<tty::SignalMask> {
    tty::SignalMask::current()
}
pub(crate) fn pty_options(
    shell: Option<(String, Vec<String>)>,
    working_directory: Option<PathBuf>,
    env: impl IntoIterator<Item = (String, String)>,
    #[cfg(not(windows))] child_signal_mask: Option<tty::SignalMask>,
    #[cfg(windows)] escape_args: bool,
) -> tty::Options {
    tty::Options {
        shell: shell.map(|(program, args)| tty::Shell::new(program, args)),
        working_directory,
        drain_on_exit: true,
        env: env.into_iter().collect(),
        #[cfg(not(windows))]
        child_signal_mask,
        #[cfg(windows)]
        escape_args,
    }
}
pub(crate) fn open_pty(
    options: &tty::Options,
    bounds: TerminalBounds,
    window_id: u64,
) -> io::Result<tty::Pty> {
    tty::new(options, window_size(bounds), window_id)
}
#[cfg(unix)]
impl From<&tty::Pty> for ProcessIdGetter {
    fn from(pty: &tty::Pty) -> Self {
        use std::os::fd::AsRawFd;
        Self::new(pty.file().as_raw_fd(), pty.child().id())
    }
}
#[cfg(windows)]
impl From<&tty::Pty> for ProcessIdGetter {
    fn from(pty: &tty::Pty) -> Self {
        use windows::Win32::{Foundation::HANDLE, System::Threading::GetProcessId};
        let child = pty.child_watcher();
        let pid = child
            .pid()
            .map(u32::from)
            .unwrap_or_else(|| unsafe { GetProcessId(HANDLE(child.raw_handle() as _)) });
        Self::new(child.raw_handle() as i32, pid)
    }
}
pub(crate) fn spawn_event_loop(
    term: Arc<BackendTermLock>,
    events: UnboundedSender<PtyEvent>,
    mut pty: tty::Pty,
    _drain_on_exit: bool,
) -> Result<PtySender> {
    let poller = Arc::new(Poller::new().context("failed to create terminal poller")?);
    // The PTY owns its descriptors until after deregistration in the worker.
    unsafe {
        pty.register(&poller, PollEvent::readable(0), PollMode::Level)?;
    }
    let (sender, receiver) = mpsc::channel();
    let worker_poller = poller.clone();
    std::thread::Builder::new()
        .name("warp-pty".into())
        .spawn(move || {
            let result = run(&term, &receiver, &worker_poller, &mut pty);
            if let Err(error) = pty.deregister(&worker_poller) {
                log::debug!("failed to deregister terminal PTY: {error}");
            }
            let event = match result {
                Ok(Some(status)) => TerminalBackendEvent::ChildExit(status),
                Ok(None) => TerminalBackendEvent::Exit,
                Err(error) => {
                    log::error!("terminal PTY failed: {error}");
                    TerminalBackendEvent::Exit
                }
            };
            if let Err(error) = events.unbounded_send(PtyEvent::Event(event, 0)) {
                log::debug!("terminal event receiver closed: {error}");
            }
        })
        .context("failed to start terminal PTY worker")?;
    Ok(PtySender { sender, poller })
}
fn run(
    term: &BackendTermLock,
    receiver: &mpsc::Receiver<Message>,
    poller: &Arc<Poller>,
    pty: &mut tty::Pty,
) -> io::Result<Option<std::process::ExitStatus>> {
    let mut parser = Processor::new();
    let mut pending = VecDeque::<Cow<'static, [u8]>>::new();
    let mut written = 0;
    let mut events = Events::new();
    let mut bytes = [0; 16 * 1024];
    loop {
        while let Ok(message) = receiver.try_recv() {
            match message {
                Message::Input(input) => {
                    if !input.is_empty() {
                        pending.push_back(input);
                    }
                }
                Message::Resize(bounds) => pty.on_resize(window_size(bounds)),
                Message::Shutdown => return Ok(None),
            }
        }
        while let Some(input) = pending.front() {
            match pty.writer().write(&input[written..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "PTY closed while writing",
                    ));
                }
                Ok(count) => {
                    written += count;
                    if written == input.len() {
                        pending.pop_front();
                        written = 0;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let child = pty.next_child_event();
        let mut received = 0;
        loop {
            match pty.reader().read(&mut bytes) {
                Ok(0) => break,
                Ok(count) => {
                    let mut term = term.lock();
                    parser.advance(&mut term, &bytes[..count]);
                    term.emit(TerminalBackendEvent::Wakeup);
                    received += count;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                // Unix PTYs report EIO after their slave closes.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => return Err(error),
            }
            if child.is_none() && received >= 64 * 1024 {
                break;
            }
        }
        if let Some(tty::ChildEvent::Exited(status)) = child {
            let mut term = term.lock();
            parser.finish_sync_output(&mut term);
            term.emit(TerminalBackendEvent::Wakeup);
            return Ok(status);
        }
        if parser
            .sync_timeout()
            .is_some_and(|timeout| timeout.is_zero())
        {
            let mut term = term.lock();
            parser.finish_sync_output(&mut term);
            term.emit(TerminalBackendEvent::Wakeup);
        }
        pty.reregister(
            poller,
            PollEvent::new(0, true, !pending.is_empty()),
            PollMode::Level,
        )?;
        events.clear();
        match poller.wait(&mut events, parser.sync_timeout()) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}
