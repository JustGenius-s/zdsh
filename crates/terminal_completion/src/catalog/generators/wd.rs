use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("wd").add_generator(
        "cat_warprc",
        Generator::script(
            CommandBuilder::single_command("cat ~/.warprc"),
            output_parsers::desc_warp_point,
        ),
    )
}
