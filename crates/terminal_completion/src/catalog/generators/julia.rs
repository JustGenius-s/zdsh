use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("julia")
        .add_filter("filter-jl", template_filters::jl())
        .add_filter("filter-dylib-so-dll", template_filters::dylib_so_dll())
}
