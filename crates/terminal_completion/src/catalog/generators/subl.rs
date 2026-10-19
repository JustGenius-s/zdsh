use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("subl")
        .add_filter("filter-sublime", template_filters::sublime())
}
