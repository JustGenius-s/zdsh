use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("tfenv")
        .add_generator(
            "list",
            Generator::script(
                CommandBuilder::single_command("tfenv list"),
                output_parsers::desc_version,
            ),
        )
        .add_generator(
            "list_remote",
            Generator::script(
                CommandBuilder::single_command("tfenv list-remote"),
                output_parsers::named_lines,
            ),
        )
}
