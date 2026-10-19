use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("python").add_filter("filter-py", template_filters::py())
}

pub fn python3_generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("python3").add_filter("filter-py", template_filters::py())
}
