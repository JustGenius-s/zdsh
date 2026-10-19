use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("rollup")
        .add_filter("filter-config-js", template_filters::config_js())
}
