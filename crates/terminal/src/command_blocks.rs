pub(crate) mod shell_startup;

pub(crate) use shell_startup::ShellStartup;

use crate::{Event, Terminal, TerminalBuilder};
use anyhow::{Result, bail};
use gpui::{AppContext, Context, Entity};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use terminal_core::model::{grid::grid_handler::GridHandler, session::SessionId};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum CommandInputState {
    Unavailable,
    Starting,
    Ready,
    Running,
    Exited,
}

#[derive(Clone)]
pub struct CommandBlock {
    pub command: String,
    pub directory: Option<PathBuf>,
    pub duration: Duration,
    /// Absent when the PTY closes without a shell completion hook.
    pub exit_code: Option<i32>,
    pub output: Entity<Terminal>,
}

pub(crate) struct ShellIntegration {
    pub session_id: SessionId,
    pub _startup: tempfile::TempDir,
    pub ready: bool,
    pub prompt: Option<terminal_core::model::ansi::PromptMetadata>,
    pub prompt_generation: u64,
    pub command: Option<String>,
    pub directory: Option<PathBuf>,
    pub started: Option<Instant>,
}

pub(crate) struct FinishedCommand {
    pub command: String,
    pub directory: Option<PathBuf>,
    pub duration: Duration,
    pub exit_code: Option<i32>,
    pub screen_cleared: bool,
    pub grid: GridHandler,
    pub colors: [Option<crate::Rgb>; 269],
}

impl Terminal {
    /// Reports whether shell hooks were installed before the PTY was started.
    pub fn supports_command_blocks(&self) -> bool {
        self.term.lock().shell.is_some()
    }

    pub fn command_input_state(&self) -> CommandInputState {
        let term = self.term.lock();
        let Some(shell) = &term.shell else {
            return CommandInputState::Unavailable;
        };
        if self.child_exited.is_some() || !self.has_active_pty_resources() {
            CommandInputState::Exited
        } else if shell.command.is_some() || !term.finished_commands.is_empty() {
            // Keep the input hidden until the foreground has installed the completed output.
            CommandInputState::Running
        } else if shell.ready {
            CommandInputState::Ready
        } else {
            CommandInputState::Starting
        }
    }

    pub fn command_blocks(&self) -> &[CommandBlock] {
        &self.command_blocks
    }

    pub fn running_command(&self) -> Option<String> {
        let term = self.term.lock();
        term.shell
            .as_ref()
            .and_then(|shell| shell.command.clone())
            .or_else(|| {
                term.finished_commands
                    .back()
                    .map(|block| block.command.clone())
            })
    }

    pub fn running_command_output_lines(&self) -> usize {
        let term = self.term.lock();
        if term
            .shell
            .as_ref()
            .is_some_and(|shell| shell.started.is_some())
            || !term.finished_commands.is_empty()
        {
            crate::warp::used_lines(&term)
                .min(crate::warp::total_lines(&term))
                .max(1)
        } else {
            // The grid still holds the previous command until the preexec hook resets it.
            0
        }
    }

    pub fn command_uses_alternate_screen(&self) -> bool {
        self.term.lock().alternate_active
    }

    pub fn command_history_line_limit(&self) -> usize {
        self.term_config.scrolling_history.max(1)
    }

    pub fn clear_command_blocks(&mut self, cx: &mut Context<Self>) {
        self.command_blocks.clear();
        cx.emit(Event::Wakeup);
    }

    /// Sends one complete editor buffer to the shell. Quoting keeps multiline text
    /// in one readline entry; eval executes it in the interactive shell's scope.
    pub fn submit_command(&mut self, command: String, cx: &mut Context<Self>) -> Result<()> {
        if command.trim().is_empty() {
            return Ok(());
        }
        if command.contains('\0') {
            bail!("Shell commands cannot contain NUL characters");
        }
        if self.command_input_state() != CommandInputState::Ready {
            bail!("The shell is not ready for another command");
        }
        let directory = self.working_directory();
        let id = {
            let mut term = self.term.lock();
            let Some(shell) = &mut term.shell else {
                bail!("Shell integration is unavailable");
            };
            shell.ready = false;
            shell.command = Some(command.clone());
            shell.directory = directory;
            shell.session_id.as_u64()
        };
        let mut quoted = String::new();
        for character in command.chars() {
            match character {
                '\\' => quoted.push_str("\\\\"),
                '\'' => quoted.push_str("\\'"),
                character if character.is_ascii_control() => {
                    use std::fmt::Write;
                    write!(quoted, "\\{:03o}", character as u32)?;
                }
                character => quoted.push(character),
            }
        }
        let hook = format!(r#"{{"hook":"Preexec","value":{{"session_id":{id},"command":""}}}}"#);
        let hook: String = hook.bytes().map(|byte| format!("{byte:02x}")).collect();
        let input = format!(
            r#" builtin printf '\033Pd{hook}\033\\'; __zdsh_record_history $'{quoted}'; builtin eval $'{quoted}'"#
        );
        self.input(format!("{input}\r").into_bytes());
        cx.emit(Event::Wakeup);
        Ok(())
    }

    pub(crate) fn finish_command_blocks(&mut self, cx: &mut Context<Self>) {
        self.term.lock().finish_command(None);
        self.collect_command_blocks(cx);
    }

    pub(crate) fn collect_command_blocks(&mut self, cx: &mut Context<Self>) {
        let (completed, cleared) = {
            let mut term = self.term.lock();
            (
                std::mem::take(&mut term.finished_commands),
                std::mem::take(&mut term.command_blocks_cleared),
            )
        };
        if cleared {
            self.command_blocks.clear();
        }
        if completed.is_empty() {
            return;
        }
        for block in completed {
            // Erased cells still occupy grid rows; a successful clear should
            // leave the next prompt, not a full-height empty output block.
            if block.screen_cleared
                && block.exit_code == Some(0)
                && block.grid.visible_content_len_for_trimming().is_none()
            {
                continue;
            }
            let mut builder = TerminalBuilder::new_display_only_with_bounds(
                self.template.cursor_shape,
                self.template.alternate_scroll,
                self.template.max_scroll_history_lines,
                self.template.window_id,
                &self.background_executor,
                self.path_style,
                self.last_content.terminal_bounds,
            );
            {
                let mut term = builder.terminal.term.lock();
                term.primary = block.grid;
                term.colors = block.colors;
            }
            builder.terminal.cwd_history = block
                .directory
                .iter()
                .map(|directory| crate::CwdHistoryEntry {
                    scrollback_position: i32::MIN,
                    working_directory: directory.clone(),
                })
                .collect();
            let output = cx.new(|cx| builder.subscribe(cx));
            self.command_blocks.push(CommandBlock {
                command: block.command,
                directory: block.directory,
                duration: block.duration,
                exit_code: block.exit_code,
                output,
            });
        }
        let limit = self.command_history_line_limit();
        let mut retained = 0usize;
        let mut first = self.command_blocks.len();
        for block in self.command_blocks.iter().rev() {
            let lines = block.output.read(cx).used_lines().saturating_add(2);
            if retained > 0 && retained.saturating_add(lines) > limit {
                break;
            }
            retained = retained.saturating_add(lines);
            first -= 1;
        }
        self.command_blocks.drain(..first);
    }
}
