// This crate was essentially pulled out verbatim from main `zed` crate to avoid having to run RustEmbed macro whenever zed has to be rebuilt. It saves a second or two on an incremental build.

use std::borrow::Cow;

use anyhow::Context as _;
use gpui::{App, AssetSource, Result, SharedString};
use gpui_kit::assets::IconName as KitIconName;

// Release builds embed the assets; dev builds read them from the checkout at
// runtime so edits show up on the next launch without a rebuild and no
// build-time path is baked in (which corgi's sandbox rejects). See
// `util::fs_embed!`.
util::fs_embed! {
    pub struct Assets,
    crate_relative = "../../assets",
    root_relative = "assets",
    include = [
        "fonts/**/*",
        "icons/**/*",
        "images/**/*",
        "themes/**/*",
        "sounds/**/*",
        "prompts/**/*",
        "*.md",
    ],
    exclude = ["themes/src/*", "*.DS_Store"],
}

/// Disambiguates Kit icon names before resolving their shared catalog entry.
const KIT_ICONS_PREFIX: &str = "kit/";

// GPUI Kit embeds only its components' icons, so the others Zed uses are embedded here.
gpui_kit::assets::icon_assets!(
    ExtraKitIcons,
    [Columns2, Sparkles, Regex, TextSearch, WholeWord]
);

/// Resolves Kit's icon name independently of legacy icons with the same name.
pub fn kit_icon_path(icon: KitIconName) -> SharedString {
    format!("{KIT_ICONS_PREFIX}{}", icon.path()).into()
}

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(icon) = gpui_icons::catalog::load(path)? {
            return Ok(Some(icon));
        }
        if let Some(kit_path) = path.strip_prefix(KIT_ICONS_PREFIX) {
            return load_kit_icon(kit_path);
        }
        if let Some(file) = Self::get(path) {
            return Ok(Some(file.data));
        }
        gpui_kit::assets::Assets
            .load(path)
            .with_context(|| format!("loading asset at path {path:?}"))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut paths: Vec<SharedString> = Self::iter()
            .filter_map(|p| {
                if p.starts_with(path) {
                    Some(p.into())
                } else {
                    None
                }
            })
            .collect();
        for kit_path in gpui_kit::assets::Assets.list(path)? {
            if Self::get(&kit_path).is_none() {
                paths.push(kit_path);
            }
        }
        for entry in gpui_icons::catalog::entries()? {
            for suffix in ["", "-filled"] {
                let icon_path = format!("gpui-icons/{}{suffix}.svg", entry.name);
                if icon_path.starts_with(path) {
                    paths.push(icon_path.into());
                }
            }
        }
        Ok(paths)
    }
}

fn load_kit_icon(path: &str) -> Result<Option<Cow<'static, [u8]>>> {
    if let Some(svg) = ExtraKitIcons.load(path)? {
        return Ok(Some(svg));
    }
    gpui_kit::assets::Assets
        .load(path)
        .with_context(|| format!("loading GPUI Kit icon at path {path:?}"))
}

impl Assets {
    /// Populate the [`TextSystem`] of the given [`AppContext`] with all `.ttf` fonts in the `fonts` directory.
    pub fn load_fonts(&self, cx: &App) -> anyhow::Result<()> {
        let font_paths = self.list("fonts")?;
        let mut embedded_fonts = Vec::new();
        for font_path in font_paths {
            if font_path.ends_with(".ttf") {
                let font_bytes = cx
                    .asset_source()
                    .load(&font_path)?
                    .expect("Assets should never return None");
                embedded_fonts.push(font_bytes);
            }
        }

        cx.text_system().add_fonts(embedded_fonts)
    }

    pub fn load_test_fonts(&self, cx: &App) {
        cx.text_system()
            .add_fonts(vec![
                self.load("fonts/lilex/Lilex-Regular.ttf").unwrap().unwrap(),
            ])
            .unwrap()
    }
}
