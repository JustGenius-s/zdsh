use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("rclone").add_generator(
        "listremotes",
        Generator::script(
            CommandBuilder::single_command("rclone listremotes"),
            output_parsers::named_lines,
        ),
    )
}
