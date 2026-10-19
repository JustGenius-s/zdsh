use super::template_filters;
use crate::metadata::CommandSignatureGenerators;

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("ts-node")
        .add_filter("filter-tsconfig", template_filters::tsconfig_json())
        .add_filter("filter-ts-tsx", template_filters::ts_tsx())
}
