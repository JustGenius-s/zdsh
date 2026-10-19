use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("softwareupdate").add_generator(
        "completions",
        Generator::script(
            CommandBuilder::single_command("softwareupdate --list"),
            output_parsers::softwareupdate_labels,
        ),
    )
}
