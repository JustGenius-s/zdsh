use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("gpg").add_generator(
        "completions",
        Generator::script(
            CommandBuilder::single_command("gpg --version"),
            output_parsers::gpg_ciphers,
        ),
    )
}
