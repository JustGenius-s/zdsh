use super::template_filters;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("vsce")
        .add_generator(
            "git_branch",
            Generator::script(
                CommandBuilder::single_command(
                    "git --no-optional-locks branch -a --no-color --sort=-committerdate",
                ),
                crate::catalog::generators::git::post_process_branches,
            ),
        )
        .add_filter("filter-vsix", template_filters::vsix())
}
