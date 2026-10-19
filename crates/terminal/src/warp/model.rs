use crate::{Point, PtyEvent, Rgb, Selection, TerminalBackendEvent};
use futures::channel::mpsc::UnboundedSender;
use std::{collections::VecDeque, io, sync::Arc};
use terminal_core::color::ColorU;
use terminal_core::{
    SizeInfo,
    event::Event as WarpEvent,
    event_listener::ChannelEventListener,
    model::{
        KeyboardModes, KeyboardModesApplyBehavior,
        ansi::{self, Handler, ProcessorInput},
        grid::{
            Dimensions,
            grid_handler::{GridHandler, PerformResetGridChecks},
        },
        index::VisibleRow,
        secrets::ObfuscateSecrets,
    },
};

pub(crate) struct BackendTerm {
    pub shell: Option<crate::command_blocks::ShellIntegration>,
    pub finished_commands: VecDeque<crate::command_blocks::FinishedCommand>,
    pub primary: GridHandler,
    pub alternate: GridHandler,
    pub alternate_active: bool,
    pub alternate_scroll: bool,
    default_cursor_style: ansi::CursorStyle,
    pub display_offset: usize,
    pub selection: Option<Selection>,
    pub vi_cursor: Option<Point>,
    pub colors: [Option<Rgb>; 269],
    size: SizeInfo,
    history_limit: usize,
    title: Option<String>,
    title_stack: VecDeque<Option<String>>,
    events: UnboundedSender<PtyEvent>,
    generation: u64,
    listener: ChannelEventListener,
    notifications: async_channel::Receiver<WarpEvent>,
    wakeups: async_channel::Receiver<()>,
}

impl BackendTerm {
    pub fn new(
        size: SizeInfo,
        history_limit: usize,
        events: UnboundedSender<PtyEvent>,
        generation: u64,
    ) -> Self {
        let (wakeups_tx, wakeups) = async_channel::unbounded();
        let (notifications_tx, notifications) = async_channel::unbounded();
        let listener = ChannelEventListener::new(wakeups_tx, notifications_tx);
        Self {
            shell: None,
            finished_commands: VecDeque::new(),
            primary: Self::new_grid(size, history_limit, listener.clone(), false),
            alternate: Self::new_grid(size, 0, listener.clone(), true),
            alternate_active: false,
            alternate_scroll: true,
            default_cursor_style: ansi::CursorStyle::default(),
            display_offset: 0,
            selection: None,
            vi_cursor: None,
            colors: [None; 269],
            size,
            history_limit,
            title: None,
            title_stack: VecDeque::new(),
            events,
            generation,
            listener,
            notifications,
            wakeups,
        }
    }

    pub fn begin_command(&mut self) {
        if let Some(shell) = &mut self.shell {
            if shell.command.is_none() || shell.started.is_some() {
                return;
            }
            shell.started = Some(std::time::Instant::now());
            self.primary =
                Self::new_grid(self.size, self.history_limit, self.listener.clone(), false);
            self.primary
                .set_cursor_style(Some(self.default_cursor_style));
            self.display_offset = 0;
            self.selection = None;
            self.vi_cursor = None;
        }
    }

    pub fn finish_command(&mut self, exit_code: Option<i32>) {
        if let Some(shell) = &mut self.shell {
            if let Some(command) = shell.command.take() {
                let started = shell.started.take();
                self.finished_commands
                    .push_back(crate::command_blocks::FinishedCommand {
                        command,
                        directory: shell.directory.take(),
                        duration: started.map(|time| time.elapsed()).unwrap_or_default(),
                        exit_code,
                        grid: if started.is_some() {
                            self.primary.clone()
                        } else {
                            Self::new_grid(
                                self.size,
                                self.history_limit,
                                self.listener.clone(),
                                false,
                            )
                        },
                        colors: self.colors,
                    });
            }
            shell.ready = true;
        }
    }

    fn new_grid(
        size: SizeInfo,
        history_limit: usize,
        listener: ChannelEventListener,
        alternate: bool,
    ) -> GridHandler {
        GridHandler::new(
            size,
            history_limit,
            listener,
            alternate,
            ObfuscateSecrets::No,
            PerformResetGridChecks::No,
        )
    }

    pub fn grid(&self) -> &GridHandler {
        if self.alternate_active {
            &self.alternate
        } else {
            &self.primary
        }
    }

    pub fn grid_mut(&mut self) -> &mut GridHandler {
        if self.alternate_active {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    pub fn set_default_cursor_style(&mut self, style: ansi::CursorStyle) {
        self.default_cursor_style = style;
        self.primary.set_cursor_style(Some(style));
        self.alternate.set_cursor_style(Some(style));
    }

    pub fn resize(&mut self, size: SizeInfo) {
        self.size = size;
        self.primary.resize(size);
        self.alternate.resize(size);
        self.display_offset = self.display_offset.min(self.grid().history_size());
        self.selection = None;
    }

    pub fn history_size(&self) -> usize {
        self.grid().history_size()
    }
    pub fn cursor_style(&self) -> ansi::CursorStyle {
        self.grid().cursor_style()
    }
    pub fn colors(&self) -> &[Option<Rgb>; 269] {
        &self.colors
    }
    pub fn cursor_line(&self) -> i32 {
        self.grid().cursor_point().row as i32 - self.history_size() as i32
    }

    pub fn emit(&self, event: TerminalBackendEvent) {
        if let Err(error) = self
            .events
            .unbounded_send(PtyEvent::Event(event, self.generation))
        {
            log::debug!("terminal event receiver closed: {error}");
        }
    }

    pub fn flush_events(&mut self) {
        // The host emits one Wakeup after each parser batch.
        while self.wakeups.try_recv().is_ok() {}
        while let Ok(event) = self.notifications.try_recv() {
            match event {
                WarpEvent::MouseCursorDirty => self.emit(TerminalBackendEvent::MouseCursorDirty),
                WarpEvent::ClipboardStore(_, text) => {
                    self.emit(TerminalBackendEvent::ClipboardStore(text))
                }
                WarpEvent::ClipboardLoad(_, format) => {
                    self.emit(TerminalBackendEvent::ClipboardLoad(format))
                }
                WarpEvent::CursorBlinkingChange(_) => {
                    self.emit(TerminalBackendEvent::CursorBlinkingChange)
                }
                WarpEvent::Bell => self.emit(TerminalBackendEvent::Bell),
                WarpEvent::ImageReceived { .. } => {
                    log::debug!("terminal inline images are not rendered")
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Processor(ansi::Processor);
impl Processor {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn advance(&mut self, term: &mut BackendTerm, bytes: &[u8]) {
        self.process(term, |processor, term, replies| {
            processor.parse_bytes(term, bytes, replies)
        });
    }
    pub fn sync_timeout(&self) -> Option<std::time::Duration> {
        self.0.sync_output_remaining_timeout()
    }
    pub fn finish_sync_output(&mut self, term: &mut BackendTerm) {
        self.process(term, |processor, term, replies| {
            processor.finish_sync_output(term, replies)
        });
    }
    fn process(
        &mut self,
        term: &mut BackendTerm,
        process: impl FnOnce(&mut ansi::Processor, &mut BackendTerm, &mut Vec<u8>),
    ) {
        let history_before = term.history_size();
        let truncated_before = term.grid().num_lines_truncated();
        let mut replies = Vec::new();
        process(&mut self.0, term, &mut replies);
        let scroll = term
            .history_size()
            .saturating_add(
                term.grid()
                    .num_lines_truncated()
                    .saturating_sub(truncated_before) as usize,
            )
            .saturating_sub(history_before);
        if term.display_offset > 0 {
            term.display_offset = term
                .display_offset
                .saturating_add(scroll)
                .min(term.history_size());
        }
        if scroll > 0 {
            let top = -(term.history_size() as i32);
            if let Some(selection) = &mut term.selection {
                selection.start.point.line = selection
                    .start
                    .point
                    .line
                    .saturating_sub_unsigned(scroll as u32);
                selection.end.point.line = selection
                    .end
                    .point
                    .line
                    .saturating_sub_unsigned(scroll as u32);
                selection.head.line = selection.head.line.saturating_sub_unsigned(scroll as u32);
                if selection.start.point.line < top && selection.end.point.line < top {
                    term.selection = None;
                }
            }
            if let Some(cursor) = &mut term.vi_cursor {
                cursor.line = cursor.line.saturating_sub_unsigned(scroll as u32).max(top);
            }
        }
        term.flush_events();
        if !replies.is_empty() {
            match String::from_utf8(replies) {
                Ok(reply) => term.emit(TerminalBackendEvent::PtyWrite(reply)),
                Err(error) => {
                    log::error!("terminal produced an invalid protocol response: {error}")
                }
            }
        }
    }
}

impl Handler for BackendTerm {
    fn is_registered_session(&self, session: terminal_core::model::session::SessionId) -> bool {
        self.shell
            .as_ref()
            .is_some_and(|shell| shell.session_id == session)
    }

    fn preexec(&mut self, _data: ansi::PreexecValue) {
        self.begin_command();
    }

    fn prompt_only_precmd(&mut self, data: ansi::PromptMetadata) {
        if let Some(shell) = &mut self.shell {
            shell.prompt = Some(data);
            shell.prompt_generation += 1;
        }
    }

    fn command_finished(&mut self, data: ansi::CommandFinishedValue) {
        self.finish_command(Some(data.completion_metadata.exit_code.value()));
    }

    fn set_title(&mut self, title: Option<String>) {
        self.title = title.map(|title| title.replace('\0', ""));
        self.emit(match &self.title {
            Some(title) => TerminalBackendEvent::Title(title.clone()),
            None => TerminalBackendEvent::ResetTitle,
        });
    }
    fn push_title(&mut self) {
        if self.title_stack.len() == 4096 {
            self.title_stack.pop_front();
        }
        self.title_stack.push_back(self.title.clone());
    }
    fn pop_title(&mut self) {
        if let Some(title) = self.title_stack.pop_back() {
            self.set_title(title);
        }
    }
    fn set_color(&mut self, index: usize, color: ColorU) {
        if let Some(slot) = self.colors.get_mut(index) {
            *slot = Some(Rgb {
                r: color.r,
                g: color.g,
                b: color.b,
            });
        }
    }
    fn reset_color(&mut self, index: usize) {
        if let Some(slot) = self.colors.get_mut(index) {
            *slot = None;
        }
    }
    fn dynamic_color_sequence<W: io::Write>(
        &mut self,
        _writer: &mut W,
        code: u8,
        index: usize,
        terminator: &str,
    ) {
        let terminator = terminator.to_owned();
        self.emit(TerminalBackendEvent::ColorRequest(
            index,
            Arc::new(move |color| {
                format!(
                    "\x1b]{code};rgb:{:04x}/{:04x}/{:04x}{terminator}",
                    u16::from(color.r) * 257,
                    u16::from(color.g) * 257,
                    u16::from(color.b) * 257
                )
            }),
        ));
    }
    fn set_mode(&mut self, mode: ansi::Mode) {
        if let ansi::Mode::SwapScreen {
            save_cursor_and_clear_screen,
        } = mode
        {
            if !self.alternate_active {
                if save_cursor_and_clear_screen {
                    self.primary.save_cursor_position();
                    self.alternate = Self::new_grid(self.size, 0, self.listener.clone(), true);
                    self.alternate
                        .set_cursor_style(Some(self.default_cursor_style));
                    if !self.alternate_scroll {
                        self.alternate.unset_mode(ansi::Mode::AlternateScroll);
                    }
                }
                self.alternate_active = true;
                self.display_offset = 0;
                self.selection = None;
            }
        } else {
            self.grid_mut().set_mode(mode);
        }
    }
    fn unset_mode(&mut self, mode: ansi::Mode) {
        if let ansi::Mode::SwapScreen {
            save_cursor_and_clear_screen,
        } = mode
        {
            if self.alternate_active {
                self.alternate_active = false;
                if save_cursor_and_clear_screen {
                    self.primary.restore_cursor_position();
                }
                self.display_offset = 0;
                self.selection = None;
            }
        } else {
            self.grid_mut().unset_mode(mode);
        }
    }
    fn reset_state(&mut self) {
        self.primary = Self::new_grid(self.size, self.history_limit, self.listener.clone(), false);
        self.alternate = Self::new_grid(self.size, 0, self.listener.clone(), true);
        self.alternate_active = false;
        self.display_offset = 0;
        self.selection = None;
        self.vi_cursor = None;
        self.colors.fill(None);
        self.primary
            .set_cursor_style(Some(self.default_cursor_style));
        self.alternate
            .set_cursor_style(Some(self.default_cursor_style));
        if !self.alternate_scroll {
            self.primary.unset_mode(ansi::Mode::AlternateScroll);
            self.alternate.unset_mode(ansi::Mode::AlternateScroll);
        }
    }
    fn set_cursor_style(&mut self, argument1: Option<ansi::CursorStyle>) {
        let style = argument1.unwrap_or(self.default_cursor_style);
        self.grid_mut().set_cursor_style(Some(style))
    }
    fn set_cursor_shape(&mut self, argument1: ansi::CursorShape) {
        self.grid_mut().set_cursor_shape(argument1)
    }
    fn input(&mut self, argument1: char) {
        self.grid_mut().input(argument1)
    }
    fn goto(&mut self, argument1: VisibleRow, argument2: usize) {
        self.grid_mut().goto(argument1, argument2)
    }
    fn goto_line(&mut self, argument1: VisibleRow) {
        self.grid_mut().goto_line(argument1)
    }
    fn goto_col(&mut self, argument1: usize) {
        self.grid_mut().goto_col(argument1)
    }
    fn insert_blank(&mut self, argument1: usize) {
        self.grid_mut().insert_blank(argument1)
    }
    fn move_up(&mut self, argument1: usize) {
        self.grid_mut().move_up(argument1)
    }
    fn move_down(&mut self, argument1: usize) {
        self.grid_mut().move_down(argument1)
    }
    fn identify_terminal<W: io::Write>(&mut self, argument1: &mut W, argument2: Option<char>) {
        self.grid_mut().identify_terminal(argument1, argument2)
    }
    fn report_xtversion<W: io::Write>(&mut self, argument1: &mut W) {
        self.grid_mut().report_xtversion(argument1)
    }
    fn device_status<W: io::Write>(&mut self, argument1: &mut W, argument2: usize) {
        self.grid_mut().device_status(argument1, argument2)
    }
    fn move_forward(&mut self, argument1: usize) {
        self.grid_mut().move_forward(argument1)
    }
    fn move_backward(&mut self, argument1: usize) {
        self.grid_mut().move_backward(argument1)
    }
    fn move_down_and_cr(&mut self, argument1: usize) {
        self.grid_mut().move_down_and_cr(argument1)
    }
    fn move_up_and_cr(&mut self, argument1: usize) {
        self.grid_mut().move_up_and_cr(argument1)
    }
    fn put_tab(&mut self, argument1: u16) {
        self.grid_mut().put_tab(argument1)
    }
    fn backspace(&mut self) {
        self.grid_mut().backspace()
    }
    fn carriage_return(&mut self) {
        self.grid_mut().carriage_return()
    }
    fn linefeed(&mut self) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().linefeed()
    }
    fn bell(&mut self) {
        self.grid_mut().bell()
    }
    fn substitute(&mut self) {
        self.grid_mut().substitute()
    }
    fn newline(&mut self) {
        self.grid_mut().newline()
    }
    fn set_horizontal_tabstop(&mut self) {
        self.grid_mut().set_horizontal_tabstop()
    }
    fn scroll_up(&mut self, argument1: usize) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().scroll_up(argument1)
    }
    fn scroll_down(&mut self, argument1: usize) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().scroll_down(argument1)
    }
    fn insert_blank_lines(
        &mut self,
        argument1: usize,
    ) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().insert_blank_lines(argument1)
    }
    fn delete_lines(&mut self, argument1: usize) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().delete_lines(argument1)
    }
    fn erase_chars(&mut self, argument1: usize) {
        self.grid_mut().erase_chars(argument1)
    }
    fn delete_chars(&mut self, argument1: usize) {
        self.grid_mut().delete_chars(argument1)
    }
    fn move_backward_tabs(&mut self, argument1: u16) {
        self.grid_mut().move_backward_tabs(argument1)
    }
    fn move_forward_tabs(&mut self, argument1: u16) {
        self.grid_mut().move_forward_tabs(argument1)
    }
    fn save_cursor_position(&mut self) {
        self.grid_mut().save_cursor_position()
    }
    fn restore_cursor_position(&mut self) {
        self.grid_mut().restore_cursor_position()
    }
    fn clear_line(&mut self, argument1: ansi::LineClearMode) {
        self.grid_mut().clear_line(argument1)
    }
    fn clear_screen(&mut self, argument1: ansi::ClearMode) {
        self.selection = None;
        self.grid_mut().clear_screen(argument1)
    }
    fn clear_tabs(&mut self, argument1: ansi::TabulationClearMode) {
        self.grid_mut().clear_tabs(argument1)
    }
    fn reverse_index(&mut self) -> terminal_core::model::selection::ScrollDelta {
        self.grid_mut().reverse_index()
    }
    fn terminal_attribute(&mut self, argument1: ansi::Attr) {
        self.grid_mut().terminal_attribute(argument1)
    }
    fn set_keyboard_enhancement_flags(
        &mut self,
        mode: KeyboardModes,
        apply: KeyboardModesApplyBehavior,
    ) {
        self.grid_mut().set_keyboard_enhancement_flags(mode, apply)
    }
    fn push_keyboard_enhancement_flags(&mut self, mode: KeyboardModes) {
        self.grid_mut().push_keyboard_enhancement_flags(mode)
    }
    fn pop_keyboard_enhancement_flags(&mut self, count: u16) {
        self.grid_mut().pop_keyboard_enhancement_flags(count)
    }
    fn query_keyboard_enhancement_flags<W: io::Write>(&mut self, writer: &mut W) {
        self.grid_mut().query_keyboard_enhancement_flags(writer)
    }
    fn set_scrolling_region(&mut self, argument1: usize, argument2: Option<usize>) {
        self.grid_mut().set_scrolling_region(argument1, argument2)
    }
    fn set_keypad_application_mode(&mut self) {
        self.grid_mut().set_keypad_application_mode()
    }
    fn unset_keypad_application_mode(&mut self) {
        self.grid_mut().unset_keypad_application_mode()
    }
    fn set_active_charset(&mut self, argument1: ansi::CharsetIndex) {
        self.grid_mut().set_active_charset(argument1)
    }
    fn configure_charset(
        &mut self,
        argument1: ansi::CharsetIndex,
        argument2: ansi::StandardCharset,
    ) {
        self.grid_mut().configure_charset(argument1, argument2)
    }
    fn clipboard_store(&mut self, argument1: u8, argument2: &[u8]) {
        self.grid_mut().clipboard_store(argument1, argument2)
    }
    fn clipboard_load(&mut self, argument1: u8, argument2: &str) {
        self.grid_mut().clipboard_load(argument1, argument2)
    }
    fn decaln(&mut self) {
        self.grid_mut().decaln()
    }
    fn text_area_size_pixels<W: io::Write>(&mut self, _writer: &mut W) {
        self.emit(TerminalBackendEvent::TextAreaSizeRequest(Arc::new(
            |bounds| {
                format!(
                    "\x1b[4;{};{}t",
                    bounds.num_lines() * f32::from(bounds.line_height()) as usize,
                    bounds.num_columns() * f32::from(bounds.cell_width()) as usize
                )
            },
        )));
    }
    fn text_area_size_chars<W: io::Write>(&mut self, _writer: &mut W) {
        self.emit(TerminalBackendEvent::TextAreaSizeRequest(Arc::new(
            |bounds| format!("\x1b[8;{};{}t", bounds.num_lines(), bounds.num_columns()),
        )));
    }
    fn set_hyperlink(&mut self, hyperlink: Option<ansi::Hyperlink>) {
        self.grid_mut().set_hyperlink(hyperlink);
    }
    fn on_finish_byte_processing(&mut self, input: &ProcessorInput<'_>) {
        self.grid_mut().on_finish_byte_processing(input);
    }
}
