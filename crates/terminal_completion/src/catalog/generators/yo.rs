use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("yo").add_generator(
        "completions",
        Generator::script(
            CommandBuilder::single_command("yo --generators"),
            output_parsers::named_lines,
        ),
    )
}
