pub enum FeatureFlag {
    OscHyperlinks,
    ResizeFix,
    SequentialStorage,
    MaximizeFlatStorage,
    ITermImages,
    KittyImages,
    KittyKeyboardProtocol,
}
impl FeatureFlag {
    pub const fn is_enabled(self) -> bool {
        matches!(
            self,
            Self::OscHyperlinks
                | Self::ResizeFix
                | Self::SequentialStorage
                | Self::MaximizeFlatStorage
        )
    }
}
