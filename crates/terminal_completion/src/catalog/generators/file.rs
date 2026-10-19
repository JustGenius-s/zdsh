use super::{fig_token, output_parsers};
use crate::metadata::{CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("file").add_generator(
        "param_keys",
        Generator::command_from_tokens(fig_token::file_param_keys, output_parsers::lines),
    )
}
