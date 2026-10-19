use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("hyper").add_generator(
        "list",
        Generator::script(
            CommandBuilder::single_command("hyper list"),
            output_parsers::desc_plugin_name,
        ),
    )
}
