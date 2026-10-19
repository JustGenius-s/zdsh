use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("tsc").add_filter("filter-ts", template_filters::ts())
}
