use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("ansible-doc").add_generator(
        "completions",
        Generator::script(
            CommandBuilder::single_command("ansible-doc --list --json 2>/dev/null"),
            output_parsers::json_object_key_descriptions,
        ),
    )
}
