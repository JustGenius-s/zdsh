use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("id").add_generator(
        "dscl_users",
        Generator::script(
            CommandBuilder::single_command("dscl . -list /Users | grep -v '^_'"),
            output_parsers::named_lines,
        ),
    )
}
