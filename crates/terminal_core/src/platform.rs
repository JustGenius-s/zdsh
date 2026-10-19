#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperatingSystem {
    Linux,
    Mac,
    Windows,
    Other(Option<&'static str>),
}
impl OperatingSystem {
    pub const fn get() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else if cfg!(windows) {
            Self::Windows
        } else if cfg!(any(target_os = "linux", target_os = "freebsd")) {
            Self::Linux
        } else {
            Self::Other(None)
        }
    }
}
