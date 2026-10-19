use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("cloudflared")
        .add_filter("filter-yml-yaml", template_filters::yml_yaml())
}
