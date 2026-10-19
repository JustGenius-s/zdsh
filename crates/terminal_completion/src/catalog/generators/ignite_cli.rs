use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("ignite-cli").add_generator(
        "ls_ignite_templates",
        Generator::script(
            CommandBuilder::single_command("ls ignite/templates"),
            output_parsers::named_lines,
        ),
    )
}
