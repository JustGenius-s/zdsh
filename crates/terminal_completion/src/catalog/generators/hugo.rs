use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("hugo").add_generator(
        "ls_archetypes",
        Generator::script(
            CommandBuilder::single_command("ls ./archetypes/"),
            output_parsers::named_lines,
        ),
    )
}
