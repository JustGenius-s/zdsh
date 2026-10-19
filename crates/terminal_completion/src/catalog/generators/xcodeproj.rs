use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("xcodeproj")
        .add_filter("filter-xcodeproj", template_filters::xcodeproj())
}
