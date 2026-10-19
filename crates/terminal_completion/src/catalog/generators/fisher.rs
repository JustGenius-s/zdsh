use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("fisher").add_generator(
        "fish_fisher_list",
        Generator::script(
            CommandBuilder::single_command("fish -c 'fisher list'"),
            output_parsers::desc_plugin,
        ),
    )
}
