use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("youtube-dl")
        .add_generator(
            "pbpaste",
            Generator::script(
                CommandBuilder::single_command("pbpaste"),
                output_parsers::youtube_clipboard_url,
            ),
        )
        .add_generator(
            "flat_playlist",
            Generator::command_from_tokens(
                super::fig_token::youtube_dl_flat_playlist,
                output_parsers::youtube_dl_flat_playlist,
            ),
        )
}
