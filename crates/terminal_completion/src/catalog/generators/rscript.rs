use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("rscript").add_filter("filter-r", template_filters::r())
}
