use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("sqlite3")
        .add_filter("filter-sqlite", template_filters::sqlite())
}
