use crate::metadata::CommandSignatureGenerators;

use crate::catalog::generators::common::{dependencies_generator, get_scripts_generator};

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("bun")
        .add_generator("get_scripts_generator", get_scripts_generator())
        .add_generator("dependencies_generator", dependencies_generator())
        .add_generator(
            "npms_search",
            crate::metadata::Generator::command_from_tokens(
                crate::catalog::generators::fig_token::npms_search,
                crate::catalog::generators::output_parsers::npms_search_results,
            ),
        )
}
