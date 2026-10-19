use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("vite").add_generator(
        "ls",
        Generator::script(
            CommandBuilder::single_command(r"\ls -l1A.env.*"),
            output_parsers::lines,
        ),
    )
}
