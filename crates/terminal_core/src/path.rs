//! Shell escaping, home expansion, and terminal hyperlink paths.
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, str};
use typed_path::TypedPathBuf;
lazy_static! {


    /// Special characters to escape in POSIX-based shells. Check for the full list here:
    /// https://mywiki.wooledge.org/BashGuide/SpecialCharacters
    static ref POSIX_SHELL_ESCAPE_PATTERN: Regex =
        Regex::new(r#"([ "\$'\\#=\[\]!><|;{}()\*\?&`~]|\n|\t)"#).expect("Shell escape regex should be valid");

    /// Special characters to escape in PowerShell. Mostly the same as [`POSIX_SHELL_ESCAPE_PATTERN`]
    /// but with the following differences:
    ///
    /// Omitted:
    /// * `\` - Backslashes are not escape characters in PowerShell.
    /// * `?` - In certain positions, `?` is the ternary operator. However, it is usually plain
    /// text. Actually "?" is a built-in alias for `Where-Object`.
    /// * `~` - Tilde is treated differently in PowerShell. It _cannot_ be tilde-escaped to avoid
    /// exansion. It has to be quoted to suppress conversion to the HOME dir.
    ///
    /// Added:
    /// * `@` - The `@` sigil creates array and object literals.
    /// * `,` - This separates array elements, and its presence causes an expression to become an
    /// array.
    static ref POWERSHELL_SHELL_ESCAPE_PATTERN: Regex =
        Regex::new(r#"([ "\$'#=\[\]!><|;{}()\*&`@,]|\n|\t)"#).expect("Shell escape regex should be valid");

    /// Regex for valid line and column number formats.
    static ref LINE_AND_COLUMN_REGEX: Vec<Regex> = vec![
        Regex::new(":(\\d+)").expect("Regex is valid"), // e.g. ":100".
        Regex::new(":(\\d+)-(?:\\d+)").expect("Regex is valid"), // e.g. ":100-200".
        Regex::new(":(\\d+):(\\d+)").expect("Regex is valid"), // e.g. ":100:300".
        Regex::new("\\[(\\d+), ?(\\d+)]").expect("Regex is valid"), // e.g. "[100, 300]".
        Regex::new("\", line (\\d+), column (\\d+)").expect("Regex is valid"), // e.g. `", line 100, column 300`.
        Regex::new("\", line (\\d+), in").expect("Regex is valid"), // e.g. `", line 100, in`.
        Regex::new("\\((\\d+), ?(\\d+)\\)").expect("Regex is valid"), // e.g. "(100, 300)".
        Regex::new("#L(\\d+)").expect("Regex is valid"), // e.g. "#L100".
        Regex::new("#L(\\d+):(\\d+)").expect("Regex is valid"), // e.g. "#L100:300"
    ];
}

/// Leading prefix for a path to the home directory using the $HOME environment variable.
pub const HOME_DIR_ENV_VAR_PREFIX: &str = "$HOME";

/// Abbreviates the session home directory in the given path to '~', if it is in the given path,
/// otherwise returns the path unchanged.
pub fn user_friendly_path<'a>(path: &'a str, home_dir: Option<&str>) -> Cow<'a, str> {
    home_dir
        .and_then(|home| {
            if path.starts_with(home) {
                let user_friendly_path = match path.strip_prefix(home) {
                    Some("") => Cow::Owned(String::from("~")),
                    Some(path_without_home) => {
                        let next_char = path_without_home
                            .chars()
                            .next()
                            .expect("already verified `path_without_home` not empty");
                        // TODO While checking `cfg!(windows)` is usually correct for determining
                        // path separators, it doesn't acccount for WSL for example.
                        if (cfg!(windows) && (next_char == '/' || next_char == '\\'))
                            || (cfg!(unix) && next_char == '/')
                        {
                            Cow::Owned("~".to_owned() + path_without_home)
                        } else {
                            Cow::Borrowed(path)
                        }
                    }
                    None => Cow::Borrowed(path),
                };
                Some(user_friendly_path)
            } else {
                None
            }
        })
        .unwrap_or(Cow::Borrowed(path))
}

/// Expands `~` alone, or a path beginning with `~` and a session path separator, against the
/// session home.
/// Other paths, including `~user` and paths without a known home, are returned unchanged.
pub fn expand_session_home(
    path: &str,
    home_dir: Option<&str>,
    path_separators: &[char],
) -> TypedPathBuf {
    let Some(home_dir) = home_dir else {
        return TypedPathBuf::from(path);
    };
    let Some(suffix) = path.strip_prefix('~') else {
        return TypedPathBuf::from(path);
    };
    if suffix.is_empty()
        || suffix
            .chars()
            .next()
            .is_some_and(|separator| path_separators.contains(&separator))
    {
        TypedPathBuf::from(format!("{home_dir}{suffix}"))
    } else {
        TypedPathBuf::from(path)
    }
}

/// Result after parsing a path string that mixes path and line and column numbers
/// into each individual components.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanPathResult {
    pub path: String,
    pub line_and_column_num: Option<LineAndColumnArg>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LineAndColumnArg {
    // line number must exist for the LineAndColumnArg.
    pub line_num: usize,
    pub column_num: Option<usize>,
}

impl LineAndColumnArg {
    pub fn to_string_suffix(&self) -> String {
        match self {
            LineAndColumnArg {
                line_num,
                column_num: Some(column_num),
            } => {
                format!(":{line_num}:{column_num}")
            }
            LineAndColumnArg {
                line_num,
                column_num: None,
            } => {
                format!(":{line_num}")
            }
        }
    }
}

impl CleanPathResult {
    /// Given a path string that contains a mix of path, line and column numbers,
    /// parse it into each individual component if the format is supported. Note
    /// that we only break it down when the whole string, rather than only part of
    /// the string, matches the format.
    pub fn with_line_and_column_number(path: &str) -> Self {
        let mut line_num = None;
        let mut column_num = None;
        let mut cleaned_path = path;
        for rg in LINE_AND_COLUMN_REGEX.iter() {
            match rg.captures(path) {
                // Need to match the entire running string rather than just part of it.
                Some(captured)
                    if captured.get(0).expect("First group always exists").end() == path.len() =>
                {
                    line_num = captured.get(1).and_then(|m| m.as_str().parse().ok());
                    column_num = captured.get(2).and_then(|m| m.as_str().parse().ok());
                    cleaned_path =
                        &path[..captured.get(0).expect("First group always exists").start()];
                }
                _ => (),
            }
        }

        Self {
            path: cleaned_path.to_owned(),
            line_and_column_num: line_num.map(|line_num| LineAndColumnArg {
                line_num,
                column_num,
            }),
        }
    }
}

/// Which character is used to escape, e.g. "\n"?
#[derive(Clone, Copy, Debug)]
pub enum EscapeChar {
    Backslash,
    Backtick,
}

impl EscapeChar {
    pub fn is_char(&self, c: char) -> bool {
        match self {
            Self::Backslash => c == '\\',
            Self::Backtick => c == '`',
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Grouping of shells with related escaping behavior.
pub enum ShellFamily {
    /// Bash, Zsh, and Fish
    Posix,
    PowerShell,
}

impl ShellFamily {
    pub fn escape_char(&self) -> EscapeChar {
        match self {
            Self::Posix => EscapeChar::Backslash,
            Self::PowerShell => EscapeChar::Backtick,
        }
    }

    /// Escapes an input string so they will retain its meaning in a no-quote representation. This
    /// is done by prepending the escape character to special/meta characters like *, |, $, etc.
    pub fn escape<'s>(&self, input: &'s str) -> Cow<'s, str> {
        if input.is_empty() {
            return "''".into();
        }
        match self {
            Self::Posix => POSIX_SHELL_ESCAPE_PATTERN.replace_all(input, "\\$1"),
            Self::PowerShell => POWERSHELL_SHELL_ESCAPE_PATTERN.replace_all(input, "`$1"),
        }
    }

    /// Unescapes a shell-escaped string by removing escape characters that were prepended to
    /// special/meta characters. This is the inverse of [`Self::escape`].
    ///
    /// Returns [`Cow::Borrowed`] when the input contains no escape characters.
    pub fn unescape<'s>(&self, input: &'s str) -> Cow<'s, str> {
        let escape_char = self.escape_char();
        if !input.contains(|c| escape_char.is_char(c)) {
            return Cow::Borrowed(input);
        }

        let mut result = String::with_capacity(input.len());
        let mut chars = input.chars();
        while let Some(c) = chars.next() {
            if escape_char.is_char(c) {
                match chars.next() {
                    Some(next) => result.push(next),
                    // Trailing escape char with nothing after it; keep as-is.
                    None => result.push(c),
                }
            } else {
                result.push(c);
            }
        }
        Cow::Owned(result)
    }

    /// Escapes the path to treat it as a single word within the shell.
    ///
    /// This function returns a [`Cow::Borrowed`] of the input string where possible and only
    /// returns owned data when the escaped version differs from the input string.
    pub fn shell_escape<'s>(&self, path: &'s str) -> Cow<'s, str> {
        // Special case if the path starts with "~/" or "~\": The escape function escapes the "~" to avoid
        // tilde expansion, but we still want tilde expansion with the rest of the path properly
        // escaped.
        for prefix in ["~", HOME_DIR_ENV_VAR_PREFIX] {
            if let Some(suffix) = path.strip_prefix(prefix) {
                if suffix.is_empty() {
                    return prefix.into();
                }
                let first_char = suffix.chars().next().expect("length already validated");
                return if first_char != '/' && first_char != '\\' {
                    self.escape(path)
                } else {
                    let escaped_sufix = self.escape(suffix);
                    // If there was no escaping to do, we can return the original path.
                    if matches!(escaped_sufix, Cow::Borrowed(_)) {
                        path.into()
                    } else {
                        Cow::Owned(format!("{prefix}{escaped_sufix}"))
                    }
                };
            }
        }
        self.escape(path)
    }
}

/// Returns `true` iff the given string is a valid POSIX portable pathname.
/// Source: https://pubs.opengroup.org/onlinepubs/9699919799/basedefs/V1_chap03.html#tag_03_271
pub fn is_posix_portable_pathname(s: &str) -> bool {
    s.split('/').all(|filename| {
        filename
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
    })
}

pub fn file_uri_drive_path_to_windows(path: &str) -> Cow<'_, str> {
    let bytes = path.as_bytes();
    let is_drive_path = bytes.len() >= 3
        && bytes[0] == b'/'
        && bytes[1].is_ascii_alphabetic()
        && bytes[2] == b':'
        && (bytes.len() == 3 || bytes[3] == b'/');
    if is_drive_path {
        Cow::Owned(path[1..].replace('/', "\\"))
    } else {
        Cow::Borrowed(path)
    }
}
