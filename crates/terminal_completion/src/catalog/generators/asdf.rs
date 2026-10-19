use crate::metadata::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResultsCollector, Suggestion,
};

/// Extracts the plugin name from the token list.
///
/// When there is trailing whitespace the last token is the plugin name (the user is completing
/// a new argument). Without trailing whitespace the last token is a partial version string and
/// the plugin name is the token before it.
fn plugin_from_tokens<'a>(tokens: &'a [&str], has_trailing_whitespace: bool) -> Option<&'a str> {
    if has_trailing_whitespace {
        tokens.last().copied()
    } else {
        tokens
            .len()
            .checked_sub(2)
            .and_then(|i| tokens.get(i))
            .copied()
    }
}

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("asdf")
        .add_generator(
            "available_plugins",
            Generator::script(
                CommandBuilder::single_command("asdf plugin list all"),
                |output| {
                    output
                        .trim()
                        .lines()
                        .filter_map(|line| {
                            let name = line.split_whitespace().next()?;
                            if name.is_empty() {
                                None
                            } else {
                                Some(Suggestion::with_description(name, "Plugin"))
                            }
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "installed_plugins",
            Generator::script(
                CommandBuilder::single_command("asdf plugin list"),
                |output| {
                    output
                        .trim()
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(|line| Suggestion::with_description(line.trim(), "Installed plugin"))
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "installed_versions",
            Generator::command_from_tokens(
                |tokens, has_trailing_whitespace, _| match plugin_from_tokens(
                    tokens,
                    has_trailing_whitespace,
                ) {
                    Some(plugin) => CommandBuilder::single_command(format!("asdf list {plugin}")),
                    None => CommandBuilder::single_command(""),
                },
                |output| {
                    output
                        .trim()
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(|line| {
                            let version = line.trim().trim_start_matches('*').trim();
                            Suggestion::with_description(version, "Installed version")
                        })
                        .collect_unordered_results()
                },
            ),
        )
        .add_generator(
            "all_versions",
            Generator::command_from_tokens(
                |tokens, has_trailing_whitespace, _| match plugin_from_tokens(
                    tokens,
                    has_trailing_whitespace,
                ) {
                    Some(plugin) => {
                        CommandBuilder::single_command(format!("asdf list all {plugin}"))
                    }
                    None => CommandBuilder::single_command(""),
                },
                |output| {
                    output
                        .trim()
                        .lines()
                        .rev()
                        .filter(|line| !line.is_empty())
                        .map(|line| Suggestion::with_description(line.trim(), "Version"))
                        .collect_ordered_results()
                },
            ),
        )
        .add_generator(
            "shims",
            Generator::script(
                CommandBuilder::single_command("ls -1 \"${ASDF_DATA_DIR:-$HOME/.asdf}/shims\""),
                |output| {
                    output
                        .trim()
                        .lines()
                        .filter(|line| !line.is_empty())
                        .map(|line| Suggestion::with_description(line.trim(), "Shim"))
                        .collect_unordered_results()
                },
            ),
        )
}
