use super::{fig_token, output_parsers};
use crate::metadata::{CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("esbuild").add_generator(
        "loader",
        Generator::command_from_tokens(fig_token::esbuild_loader, output_parsers::lines),
    )
}
