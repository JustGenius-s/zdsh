use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("pm2").add_filter("filter-json", template_filters::json())
}
