use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("quickmail").add_generator(
        "template_listall",
        Generator::script(
            CommandBuilder::single_command("quickmail template listall"),
            output_parsers::named_lines,
        ),
    )
}
