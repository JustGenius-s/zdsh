use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("elm-review").add_generator(
        "echo",
        Generator::script(
            CommandBuilder::single_command("echo"),
            output_parsers::named_lines,
        ),
    )
}
