use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("mackup").add_generator(
        "list",
        Generator::script(
            CommandBuilder::single_command("mackup list"),
            output_parsers::named_lines,
        ),
    )
}
