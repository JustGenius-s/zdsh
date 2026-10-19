use crate::Terminal;
use anyhow::{Result, bail};
use async_trait::async_trait;
use parking_lot::Mutex;
use std::{
    collections::{BTreeSet, HashMap},
    path::PathBuf,
    sync::Arc,
};
use terminal_completion::{
    completer::{
        CompletionContext, EngineDirEntry, GeneratorContext, PathCompletionContext, PathSeparators,
    },
    signatures::CommandRegistry,
};
use terminal_core::path::ShellFamily;
use typed_path::{TypedPath, TypedPathBuf};

pub use terminal_completion::{CommandCompletion, history_suggestion};

/// A local terminal's launch environment and current directory. Query on a background executor.
pub struct CommandCompletionRequest {
    directory: PathBuf,
    environment: HashMap<String, String>,
}

impl Terminal {
    pub fn command_completion_request(&self) -> Option<CommandCompletionRequest> {
        Some(CommandCompletionRequest {
            directory: self.working_directory()?,
            environment: std::env::vars().chain(self.template.env.clone()).collect(),
        })
    }
}

struct LocalCompletionContext {
    request: CommandCompletionRequest,
    registry: Arc<CommandRegistry>,
    commands: BTreeSet<String>,
    error: Mutex<Option<String>>,
}

impl CommandCompletionRequest {
    /// Uses Warp's parser, embedded command signatures, matching, and shell escaping.
    /// Dynamic command generators are disabled; completion never executes the input buffer.
    pub async fn query(self, text: &str, cursor: usize) -> Result<Vec<CommandCompletion>> {
        if !text.is_char_boundary(cursor) {
            bail!("The completion cursor is not on a character boundary.");
        }
        let mut commands = BTreeSet::new();
        if let Some(path) = self.environment.get("PATH") {
            for directory in std::env::split_paths(path) {
                let directory = self.directory.join(directory);
                match std::fs::read_dir(&directory) {
                    Ok(entries) => {
                        for entry in entries {
                            let entry = match entry {
                                Ok(entry) => entry,
                                Err(error) => {
                                    log::debug!("Cannot read command entry: {error}");
                                    continue;
                                }
                            };
                            if is_executable(&entry.path()) {
                                commands.insert(entry.file_name().to_string_lossy().into_owned());
                            }
                        }
                    }
                    Err(error) => log::debug!(
                        "Cannot read PATH directory {}: {error}",
                        directory.display()
                    ),
                }
            }
        }
        // These builtins are shared by the Bash and Zsh shells supported by command blocks.
        commands.extend(
            [
                "alias", "bg", "cd", "command", "echo", "eval", "exec", "exit", "export", "false",
                "fg", "jobs", "kill", "printf", "pwd", "read", "readonly", "return", "set",
                "shift", "source", "test", "true", "type", "ulimit", "umask", "unalias", "unset",
                "wait",
            ]
            .map(str::to_owned),
        );
        let context = LocalCompletionContext {
            request: self,
            registry: CommandRegistry::global_instance(),
            commands,
            error: Mutex::new(None),
        };
        let suggestions = terminal_completion::complete(
            text,
            cursor,
            Some(&context.request.environment),
            &context,
        )
        .await?;
        if let Some(error) = context.error.lock().take() {
            bail!(error);
        }
        Ok(suggestions)
    }
}

fn is_executable(path: &std::path::Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

impl CompletionContext for LocalCompletionContext {
    fn path_completion_context(&self) -> Option<&dyn PathCompletionContext> {
        Some(self)
    }
    fn generator_context(&self) -> Option<&dyn GeneratorContext> {
        None
    }
    fn top_level_commands(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(self.commands.iter().map(String::as_str))
    }
    fn command_registry(&self) -> &CommandRegistry {
        &self.registry
    }
    fn shell_family(&self) -> Option<ShellFamily> {
        Some(ShellFamily::Posix)
    }
}

#[async_trait]
impl PathCompletionContext for LocalCompletionContext {
    async fn list_directory_entries(&self, directory: TypedPathBuf) -> Arc<Vec<EngineDirEntry>> {
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(directory.as_bytes()))
        };
        #[cfg(not(unix))]
        let path = PathBuf::from(directory.to_string_lossy().into_owned());
        let entries = std::fs::read_dir(&path).and_then(|entries| {
            entries
                .map(|entry| entry.and_then(EngineDirEntry::try_from))
                .collect::<std::io::Result<Vec<_>>>()
        });
        match entries {
            Ok(entries) => Arc::new(entries),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    *self.error.lock() = Some(format!(
                        "Cannot complete paths in {}: {error}",
                        path.display()
                    ));
                }
                Arc::new(Vec::new())
            }
        }
    }
    fn home_directory(&self) -> Option<&str> {
        self.request.environment.get("HOME").map(String::as_str)
    }
    fn cdpath(&self) -> Option<&str> {
        self.request.environment.get("CDPATH").map(String::as_str)
    }
    fn shell_family(&self) -> ShellFamily {
        ShellFamily::Posix
    }
    fn pwd(&self) -> TypedPath<'_> {
        TypedPath::unix(self.request.directory.as_os_str().as_encoded_bytes())
    }
    fn path_separators(&self) -> PathSeparators {
        PathSeparators::for_unix()
    }
}
