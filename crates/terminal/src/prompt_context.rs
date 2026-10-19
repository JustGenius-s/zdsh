use crate::{Terminal, command_blocks::shell_startup::CONTEXT_ENVIRONMENT};
use anyhow::{Context as _, Result, bail};
use gpui::BackgroundExecutor;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

#[derive(Clone, Default)]
pub struct PromptContext {
    pub directory: String,
    pub branch: Option<String>,
    pub runtimes: Vec<String>,
    pub errors: Vec<String>,
}

pub struct PromptContextRequest {
    pub generation: u64,
    pub directory: PathBuf,
    environment: HashMap<String, String>,
}

impl Terminal {
    pub fn prompt_context_request(&self) -> Option<PromptContextRequest> {
        let term = self.term.lock();
        let shell = term.shell.as_ref()?;
        let metadata = shell.prompt.as_ref()?;
        let directory = PathBuf::from(metadata.pwd.as_ref()?);
        let mut environment: HashMap<_, _> =
            std::env::vars().chain(self.template.env.clone()).collect();
        if let Some(current) = &metadata.zdsh_environment {
            for name in CONTEXT_ENVIRONMENT {
                match current.get(*name) {
                    Some(value) if !value.is_empty() => {
                        environment.insert((*name).into(), value.clone());
                    }
                    _ => {
                        environment.remove(*name);
                    }
                }
            }
        }
        Some(PromptContextRequest {
            generation: shell.prompt_generation,
            directory,
            environment,
        })
    }
}

impl PromptContextRequest {
    pub fn display_directory(&self) -> String {
        if let Some(home) = self.environment.get("HOME")
            && let Ok(relative) = self.directory.strip_prefix(home)
        {
            if relative.as_os_str().is_empty() {
                return "~".into();
            }
            return format!("~/{}", relative.display());
        }
        self.directory.to_string_lossy().into_owned()
    }

    /// Queries only project-relevant tools using the shell's most recent environment.
    /// Run on a background executor; timeout or cancellation terminates each direct child.
    pub async fn query(self, executor: BackgroundExecutor) -> PromptContext {
        let mut context = PromptContext {
            directory: self.display_directory(),
            ..Default::default()
        };
        let rust = self.has_project_file(&["Cargo.toml", "rust-toolchain", "rust-toolchain.toml"]);
        let node = self.has_project_file(&["package.json", ".node-version", ".nvmrc"]);
        let python = self.environment.contains_key("VIRTUAL_ENV")
            || self.environment.contains_key("CONDA_DEFAULT_ENV")
            || self.has_project_file(&["pyproject.toml", "requirements.txt", ".python-version"]);
        let (branch, rust, node, python) = futures::join!(
            self.git_branch(&executor),
            self.version(rust, "rustc", &executor),
            self.version(node, "node", &executor),
            self.version(python, "python3", &executor),
        );
        match branch {
            Ok(branch) => context.branch = branch,
            Err(error) => context.errors.push(error.to_string()),
        }
        for (name, result) in [("Rust", rust), ("Node", node), ("Python", python)] {
            match result {
                Ok(Some(version)) => {
                    let version = version
                        .strip_prefix("rustc ")
                        .or_else(|| version.strip_prefix("Python "))
                        .unwrap_or(&version);
                    let version = version
                        .split_whitespace()
                        .next()
                        .unwrap_or(version)
                        .trim_start_matches('v');
                    context.runtimes.push(format!("{name} {version}"));
                }
                Ok(None) => {}
                Err(error) => context.errors.push(format!("{name}: {error}")),
            }
        }
        if let Some(environment) = self
            .environment
            .get("VIRTUAL_ENV")
            .and_then(|path| Path::new(path).file_name())
            .and_then(|name| name.to_str())
        {
            context.runtimes.push(environment.to_owned());
        } else if let Some(environment) = self.environment.get("CONDA_DEFAULT_ENV") {
            context.runtimes.push(environment.clone());
        }
        context
    }

    fn has_project_file(&self, names: &[&str]) -> bool {
        self.directory
            .ancestors()
            .any(|directory| names.iter().any(|name| directory.join(name).is_file()))
    }

    async fn version(
        &self,
        enabled: bool,
        program: &str,
        executor: &BackgroundExecutor,
    ) -> Result<Option<String>> {
        if !enabled {
            return Ok(None);
        }
        self.output(program, &["--version"], executor).await
    }

    async fn git_branch(&self, executor: &BackgroundExecutor) -> Result<Option<String>> {
        if let Some(branch) = self
            .output(
                "git",
                &["symbolic-ref", "--quiet", "--short", "HEAD"],
                executor,
            )
            .await?
        {
            return Ok(Some(branch));
        }
        self.output("git", &["rev-parse", "--short", "HEAD"], executor)
            .await
    }

    async fn output(
        &self,
        program: &str,
        args: &[&str],
        executor: &BackgroundExecutor,
    ) -> Result<Option<String>> {
        let mut command = smol::process::Command::new(program);
        command
            .args(args)
            .current_dir(&self.directory)
            .env_clear()
            .envs(&self.environment)
            .env("RUSTUP_AUTO_INSTALL", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("Cannot read {program} environment"));
            }
        };
        let output = futures_lite::future::race(
            async { child.output().await.context("Environment query failed") },
            async {
                executor.timer(Duration::from_secs(2)).await;
                bail!("{program} environment query timed out")
            },
        )
        .await?;
        if !output.status.success() {
            if program == "git" {
                return Ok(None);
            }
            bail!("Version unavailable (exit {})", output.status);
        }
        let output =
            String::from_utf8(output.stdout).context("Invalid environment query output")?;
        Ok(output
            .lines()
            .next()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned))
    }
}
