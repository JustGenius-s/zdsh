use super::ShellIntegration;
use anyhow::{Context as _, Result};
use collections::HashMap;
use std::{fs, path::Path};
use task::Shell;
use tempfile::TempDir;
use terminal_core::{model::session::SessionId, session_id::generate_session_id};

pub(crate) const CONTEXT_ENVIRONMENT: &[&str] = &[
    "PATH",
    "HOME",
    "HISTFILE",
    "ZSH_VERSION",
    "BASH_VERSION",
    "RUSTUP_HOME",
    "CARGO_HOME",
    "RUSTUP_TOOLCHAIN",
    "VIRTUAL_ENV",
    "CONDA_DEFAULT_ENV",
    "PYENV_VERSION",
    "PYENV_ROOT",
    "ASDF_DATA_DIR",
    "MISE_DATA_DIR",
    "MISE_CONFIG_DIR",
];

pub(crate) struct ShellStartup {
    directory: TempDir,
    session_id: SessionId,
    program: String,
    args: Vec<String>,
    zsh: bool,
}

impl ShellStartup {
    pub fn prepare(
        shell: &Shell,
        environment: &HashMap<String, String>,
        activation: &[String],
    ) -> Result<Option<Self>> {
        let (program, args) = shell.program_and_args();
        let name = Path::new(&program)
            .file_name()
            .and_then(|name| name.to_str());
        if !matches!(name, Some("bash" | "zsh"))
            || args
                .iter()
                .any(|arg| !matches!(arg.as_str(), "-i" | "-l" | "-il" | "-li" | "--login"))
        {
            return Ok(None);
        }
        let zsh = name == Some("zsh");
        let login = (cfg!(target_os = "macos") && matches!(shell, Shell::System))
            || args.iter().any(|arg| arg.contains('l'));
        let directory = tempfile::Builder::new()
            .prefix("zdsh-shell-")
            .tempdir()
            .context("failed to create terminal shell startup directory")?;
        let session_id = generate_session_id();
        let hook = shell_hook(session_id, zsh);
        let initialization = format!("{}\n{hook}\n", activation.join("\n"));
        let args = if zsh {
            write_zsh_startup(directory.path(), environment, &initialization)?;
            vec![if login { "-il" } else { "-i" }.into()]
        } else {
            // Bash ignores --rcfile in login mode. Source the login profiles from
            // the managed rcfile, as shell-integration hosts do, before our hooks.
            let profiles = if login {
                r#"[[ ! -r /etc/profile ]] || builtin source /etc/profile
for __zdsh_profile in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
    if [[ -r "$__zdsh_profile" ]]; then
        builtin source "$__zdsh_profile"
        break
    fi
done
unset __zdsh_profile"#
            } else {
                r#"[[ ! -r "$HOME/.bashrc" ]] || builtin source "$HOME/.bashrc""#
            };
            let rcfile = directory.path().join("bashrc");
            fs::write(&rcfile, format!("{profiles}\n{initialization}"))
                .context("failed to write Bash startup integration")?;
            vec![
                "--rcfile".into(),
                rcfile.to_string_lossy().into_owned(),
                "-i".into(),
            ]
        };
        Ok(Some(Self {
            directory,
            session_id,
            program,
            args,
            zsh,
        }))
    }

    pub fn into_launch(
        self,
        environment: &mut HashMap<String, String>,
    ) -> (String, Vec<String>, ShellIntegration) {
        if self.zsh {
            environment.insert(
                "ZDOTDIR".into(),
                self.directory.path().to_string_lossy().into_owned(),
            );
        }
        let integration = ShellIntegration {
            session_id: self.session_id,
            _startup: self.directory,
            ready: false,
            prompt: None,
            prompt_generation: 0,
            command: None,
            directory: None,
            started: None,
        };
        (self.program, self.args, integration)
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_zsh_startup(
    directory: &Path,
    environment: &HashMap<String, String>,
    initialization: &str,
) -> Result<()> {
    let original = environment
        .get("ZDOTDIR")
        .cloned()
        .or_else(|| std::env::var("ZDOTDIR").ok());
    let restore_original = match original {
        Some(value) => format!("export ZDOTDIR={}", quote(&value)),
        None => "unset ZDOTDIR".into(),
    };
    let restore = r#"if (( __zdsh_zdotdir_set )); then
    export ZDOTDIR="$__zdsh_zdotdir"
else
    unset ZDOTDIR
fi"#;
    let redirect = format!(
        "__zdsh_zdotdir_set=${{+ZDOTDIR}}\n__zdsh_zdotdir=${{ZDOTDIR-}}\nexport ZDOTDIR={}\n",
        quote(&directory.to_string_lossy())
    );
    for filename in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
        let restore = if filename == ".zshenv" {
            restore_original.as_str()
        } else {
            restore
        };
        // macOS /etc/zshrc derives HISTFILE from our temporary ZDOTDIR before
        // this wrapper runs. Restore that default before loading the user's rc.
        let history = if filename == ".zshrc" {
            format!(
                "if [[ ${{HISTFILE-}} == {} ]]; then\n    HISTFILE=\"${{ZDOTDIR:-$HOME}}/.zsh_history\"\nfi\n",
                quote(&directory.join(".zsh_history").to_string_lossy())
            )
        } else {
            String::new()
        };
        let load = format!(
            "{restore}\n{history}[[ ! -r \"${{ZDOTDIR-$HOME}}/{filename}\" ]] || builtin source \"${{ZDOTDIR-$HOME}}/{filename}\"\n"
        );
        let finish = format!("unset __zdsh_zdotdir_set __zdsh_zdotdir\n{initialization}");
        let tail = match filename {
            ".zshrc" => format!("if [[ -o login ]]; then\n{redirect}\nelse\n{finish}\nfi\n"),
            ".zlogin" => finish,
            _ => redirect.clone(),
        };
        fs::write(directory.join(filename), format!("{load}{tail}"))
            .with_context(|| format!("failed to write Zsh startup integration {filename}"))?;
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use smol::process::Command;

    #[test]
    fn zsh_startup_restores_history_path_before_user_configuration() -> Result<()> {
        let home = tempfile::tempdir()?;
        let startup = tempfile::tempdir()?;
        fs::write(
            home.path().join(".zshrc"),
            "[[ -n $HISTFILE ]] || HISTFILE=$HOME/.zsh_history\n",
        )?;
        write_zsh_startup(
            startup.path(),
            &HashMap::from_iter([("ZDOTDIR".into(), home.path().to_string_lossy().into_owned())]),
            "",
        )?;
        for login in [false, true] {
            let output = smol::block_on(
                Command::new("/bin/zsh")
                    .env_clear()
                    .env("HOME", home.path())
                    .env("ZDOTDIR", startup.path())
                    .env("PATH", "/usr/bin:/bin")
                    .env("TERM", "dumb")
                    .arg(if login { "-ilc" } else { "-ic" })
                    .arg("printf '%s' \"$HISTFILE\"; SAVEHIST=0")
                    .output(),
            )?;
            assert!(output.status.success(), "{:?}", output.stderr);
            assert_eq!(
                String::from_utf8(output.stdout)?,
                home.path().join(".zsh_history").to_string_lossy()
            );
        }
        fs::write(
            home.path().join(".zshrc"),
            "HISTFILE=$HOME/custom_history\n",
        )?;
        let output = smol::block_on(
            Command::new("/bin/zsh")
                .env_clear()
                .env("HOME", home.path())
                .env("ZDOTDIR", startup.path())
                .env("PATH", "/usr/bin:/bin")
                .env("TERM", "dumb")
                .args(["-ic", "printf '%s' \"$HISTFILE\"; SAVEHIST=0"])
                .output(),
        )?;
        assert!(output.status.success(), "{:?}", output.stderr);
        assert_eq!(
            String::from_utf8(output.stdout)?,
            home.path().join("custom_history").to_string_lossy()
        );
        Ok(())
    }
}

fn shell_hook(session_id: SessionId, zsh: bool) -> String {
    let id = session_id.as_u64();
    let emit = include_str!("prompt.sh");
    let mut context = format!(
        r#"__zdsh_context() {{
    local __zdsh_json_result __zdsh_context_json
    __zdsh_json "$PWD"
    __zdsh_context_json='{{"hook":"Precmd","value":{{"session_id":{id},"pwd":'"$__zdsh_json_result"',"zdsh_environment":{{'
"#
    );
    for (index, name) in CONTEXT_ENVIRONMENT.iter().enumerate() {
        let separator = if index == 0 { "" } else { "," };
        context.push_str(&format!(
            r#"    __zdsh_json "${{{name}-}}"
    __zdsh_context_json="$__zdsh_context_json"'{separator}"{name}":'"$__zdsh_json_result"
"#
        ));
    }
    context.push_str("    __zdsh_emit \"$__zdsh_context_json}}}\"\n}\n");
    let hook = format!(
        r#"{emit}
{context}
__zdsh_precmd() {{ local __zdsh_status=$?; __zdsh_context; __zdsh_emit '{{"hook":"CommandFinished","value":{{"session_id":{id},"exit_code":'"$__zdsh_status"',"next_block_id":""}}}}'; return "$__zdsh_status"; }}"#
    );
    if zsh {
        format!(
            r#"setopt HIST_IGNORE_SPACE
__zdsh_record_history() {{
    [[ -n "$1" && "$1" != [[:space:]]* ]] || return 0
    builtin print -s -- "$1"
    [[ -z "${{HISTFILE-}}" ]] || builtin fc -AI
}}
{hook}
precmd_functions=(__zdsh_precmd ${{precmd_functions:#__zdsh_precmd}})"#
        )
    } else {
        format!(
            r#"HISTCONTROL="${{HISTCONTROL:+$HISTCONTROL:}}ignorespace"
__zdsh_record_history() {{
    [[ -n "$1" && "$1" != [[:space:]]* ]] || return 0
    builtin history -s -- "$1"
    [[ -z "${{HISTFILE-}}" ]] || builtin history -a
}}
{hook}
if [[ $(builtin declare -p PROMPT_COMMAND 2>/dev/null) == 'declare -a '* ]]; then
    PROMPT_COMMAND=(__zdsh_precmd "${{PROMPT_COMMAND[@]}}")
else
    PROMPT_COMMAND="__zdsh_precmd${{PROMPT_COMMAND:+; $PROMPT_COMMAND}}"
fi"#
        )
    }
}
