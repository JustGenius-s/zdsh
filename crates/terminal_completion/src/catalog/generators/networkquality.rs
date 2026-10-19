use super::output_parsers;
use crate::metadata::{CommandBuilder, CommandSignatureGenerators, Generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("networkquality").add_generator(
        "networksetup",
        Generator::script(
            CommandBuilder::single_command("networksetup -listallhardwareports"),
            output_parsers::networksetup_ports,
        ),
    )
}
