use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("copilot").add_generator(
        "cat_copilot_workspace",
        Generator::script(
            CommandBuilder::single_command("cat copilot/.workspace"),
            output_parsers::yaml_application,
        ),
    )
}
