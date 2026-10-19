use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("eb").add_generator(
        "list",
        Generator::script(
            CommandBuilder::single_command("eb list"),
            output_parsers::strip_star_prefix,
        ),
    )
}
