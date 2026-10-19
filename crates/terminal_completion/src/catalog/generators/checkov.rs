use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("checkov").add_generator(
        "git_branch",
        Generator::script(
            CommandBuilder::single_command("git branch --no-color"),
            crate::catalog::generators::git::post_process_branches,
        ),
    )
}
