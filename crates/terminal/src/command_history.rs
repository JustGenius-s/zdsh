use crate::Terminal;
use anyhow::{Context as _, Result};
use std::{
    fs::File,
    io::{Read as _, Seek as _, SeekFrom},
    path::PathBuf,
};

const MAX_HISTORY_BYTES: u64 = 8 * 1024 * 1024;
const MAX_HISTORY_COMMANDS: usize = 10_000;

#[derive(Clone, Copy)]
enum HistoryFormat {
    Zsh,
    Bash,
}

pub struct CommandHistoryRequest {
    pub generation: u64,
    path: Option<PathBuf>,
    format: HistoryFormat,
    limit: usize,
}

impl Terminal {
    pub fn command_history_request(&self) -> Option<CommandHistoryRequest> {
        if self.is_remote_terminal {
            return None;
        }
        let term = self.term.lock();
        let shell = term.shell.as_ref()?;
        CommandHistoryRequest::from_prompt(
            shell.prompt_generation,
            shell.prompt.as_ref()?,
            self.command_history_line_limit(),
        )
    }
}

impl CommandHistoryRequest {
    fn from_prompt(
        generation: u64,
        prompt: &terminal_core::model::ansi::PromptMetadata,
        limit: usize,
    ) -> Option<Self> {
        let environment = prompt.zdsh_environment.as_ref()?;
        let format = if environment
            .get("ZSH_VERSION")
            .is_some_and(|value| !value.is_empty())
        {
            HistoryFormat::Zsh
        } else if environment
            .get("BASH_VERSION")
            .is_some_and(|value| !value.is_empty())
        {
            HistoryFormat::Bash
        } else {
            return None;
        };
        // HISTFILE is often a non-exported shell variable, so use the prompt's
        // value rather than the IDE's environment or a guessed home-directory path.
        let path = environment
            .get("HISTFILE")
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = PathBuf::from(path);
                if path.is_absolute() {
                    path
                } else {
                    PathBuf::from(prompt.pwd.as_deref().unwrap_or_default()).join(path)
                }
            });
        Some(CommandHistoryRequest {
            generation,
            path,
            format,
            limit: limit.min(MAX_HISTORY_COMMANDS),
        })
    }
    pub fn load(self) -> Result<Vec<String>> {
        let Some(path) = self.path else {
            return Ok(Vec::new());
        };
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot read shell history {}", path.display()));
            }
        };
        let length = file.metadata()?.len();
        let start = length.saturating_sub(MAX_HISTORY_BYTES);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::new();
        file.take(MAX_HISTORY_BYTES)
            .read_to_end(&mut bytes)
            .with_context(|| format!("Cannot read shell history {}", path.display()))?;
        let bytes = if start > 0 {
            // Ignore the first partial record when reading the tail of a large file.
            let boundary = bytes
                .iter()
                .enumerate()
                .find_map(|(index, byte)| {
                    (*byte == b'\n' && (index == 0 || bytes.get(index - 1) != Some(&b'\\')))
                        .then_some(index + 1)
                })
                .unwrap_or(bytes.len());
            bytes.get(boundary..).unwrap_or_default()
        } else {
            bytes.as_slice()
        };
        let mut commands = match self.format {
            HistoryFormat::Zsh => parse_zsh_history(bytes),
            HistoryFormat::Bash => parse_bash_history(bytes),
        };
        commands.retain(|command| !command.trim().is_empty() && !command.contains('\0'));
        if commands.len() > self.limit {
            commands.drain(..commands.len() - self.limit);
        }
        Ok(commands)
    }
}

fn parse_zsh_history(bytes: &[u8]) -> Vec<String> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut bytes = bytes.iter().copied();
    while let Some(byte) = bytes.next() {
        // Zsh metafies bytes that overlap its internal token range, including
        // some bytes inside otherwise ordinary UTF-8 characters.
        if byte == 0x83 {
            if let Some(byte) = bytes.next() {
                decoded.push(byte ^ 32);
            }
        } else {
            decoded.push(byte);
        }
    }
    let text = String::from_utf8_lossy(&decoded);
    let mut commands = Vec::new();
    let mut command = String::new();
    for line in text.lines() {
        let line = if command.is_empty() {
            strip_zsh_timestamp(line)
        } else {
            line
        };
        if let Some(line) = line.strip_suffix('\\') {
            command.push_str(line);
            command.push('\n');
        } else {
            command.push_str(line);
            commands.push(std::mem::take(&mut command));
        }
    }
    if !command.is_empty() {
        commands.push(command);
    }
    commands
}

fn strip_zsh_timestamp(line: &str) -> &str {
    let Some((timestamp, command)) = line
        .strip_prefix(": ")
        .and_then(|line| line.split_once(';'))
    else {
        return line;
    };
    let Some((started, duration)) = timestamp.split_once(':') else {
        return line;
    };
    if !started.is_empty()
        && !duration.is_empty()
        && started
            .bytes()
            .chain(duration.bytes())
            .all(|byte| byte.is_ascii_digit())
    {
        command
    } else {
        line
    }
}

fn bash_timestamp(line: &str) -> bool {
    line.strip_prefix('#').is_some_and(|timestamp| {
        !timestamp.is_empty() && timestamp.bytes().all(|byte| byte.is_ascii_digit())
    })
}

fn parse_bash_history(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut commands = Vec::new();
    let mut command = String::new();
    let mut timestamped = false;
    for line in text.lines() {
        if bash_timestamp(line) {
            if !command.is_empty() {
                commands.push(std::mem::take(&mut command));
            }
            timestamped = true;
        } else if timestamped {
            if !command.is_empty() {
                command.push('\n');
            }
            command.push_str(line);
        } else {
            commands.push(line.to_owned());
        }
    }
    if !command.is_empty() {
        commands.push(command);
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn history_from_native_zsh_prompt_is_available_to_completion_after_restart() -> Result<()> {
        let home = tempfile::tempdir()?;
        std::fs::write(home.path().join(".zshrc"), "SAVEHIST=0\n")?;
        std::fs::write(
            home.path().join(".zsh_history"),
            ": 123:0;echo persisted-command\n",
        )?;
        for _ in 0..2 {
            let mut environment = collections::HashMap::from_iter([
                ("HOME".into(), home.path().to_string_lossy().into_owned()),
                ("ZDOTDIR".into(), home.path().to_string_lossy().into_owned()),
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("TERM".into(), "dumb".into()),
            ]);
            let startup = crate::command_blocks::ShellStartup::prepare(
                &task::Shell::Program("/bin/zsh".into()),
                &environment,
                &[],
            )?
            .context("missing shell integration")?;
            let (program, arguments, shell) = startup.into_launch(&mut environment);
            let output = smol::block_on(
                smol::process::Command::new(program)
                    .env_clear()
                    .envs(environment)
                    .args(arguments)
                    .args(["-c", "__zdsh_context"])
                    .output(),
            )?;
            assert!(output.status.success(), "{:?}", output.stderr);
            let (sender, _receiver) = futures::channel::mpsc::unbounded();
            let mut term = crate::warp::BackendTerm::new(
                crate::warp::size_info(crate::TerminalBounds::default()),
                1000,
                sender,
                0,
            );
            term.shell = Some(shell);
            crate::warp::Processor::new().advance(&mut term, &output.stdout);
            let shell = term.shell.as_ref().context("missing shell")?;
            let request = CommandHistoryRequest::from_prompt(
                shell.prompt_generation,
                shell.prompt.as_ref().context("missing prompt metadata")?,
                1000,
            )
            .context("missing history request")?;
            let history = request.load()?;
            assert_eq!(history, ["echo persisted-command"]);
            assert_eq!(
                crate::completions::history_suggestion(
                    "echo per",
                    history.iter().map(String::as_str)
                ),
                Some("echo persisted-command".into())
            );
        }
        Ok(())
    }

    #[test]
    fn zsh_history_preserves_multiline_and_metafied_unicode() {
        let mut bytes = b": 123:0;echo first\n: 124:2;echo one\\\necho two\n: 125:0;echo ".to_vec();
        for byte in "中文".bytes() {
            if byte == 0x83 || (0x84..=0x9f).contains(&byte) {
                bytes.extend([0x83, byte ^ 32]);
            } else {
                bytes.push(byte);
            }
        }
        bytes.push(b'\n');
        assert_eq!(
            parse_zsh_history(&bytes),
            ["echo first", "echo one\necho two", "echo 中文"]
        );
        assert_eq!(
            parse_zsh_history(b"echo plain\n: not:a;timestamp\n"),
            ["echo plain", ": not:a;timestamp"]
        );
    }

    #[test]
    fn bash_history_handles_plain_and_timestamped_commands() {
        assert_eq!(
            parse_bash_history(b"echo one\necho two\n"),
            ["echo one", "echo two"]
        );
        assert_eq!(
            parse_bash_history(b"old command\n#123\necho one\necho two\n#124\necho three\n"),
            ["old command", "echo one\necho two", "echo three"]
        );
    }

    #[test]
    fn history_load_uses_selected_file_and_retains_recent_commands() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("custom_history");
        std::fs::write(&path, "echo old\necho recent\necho newest\n")?;
        let request = CommandHistoryRequest {
            generation: 1,
            path: Some(path),
            format: HistoryFormat::Zsh,
            limit: 2,
        };
        assert_eq!(request.load()?, ["echo recent", "echo newest"]);
        let missing = CommandHistoryRequest {
            generation: 2,
            path: Some(directory.path().join("missing")),
            format: HistoryFormat::Bash,
            limit: 2,
        };
        assert!(missing.load()?.is_empty());
        Ok(())
    }
}
