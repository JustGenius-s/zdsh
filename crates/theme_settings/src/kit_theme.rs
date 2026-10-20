//! Themes are managed the GPUI Kit way: theme files use its `ThemeSet` format, its
//! `ThemeRegistry` lists the available themes, and its global `Theme` holds the active light and
//! dark themes and the mode. Zed's [`GlobalTheme`] is derived from that global, so code that
//! still reads `cx.theme()` keeps working while it moves to GPUI Kit's tokens.
//!
//! GPUI Kit's tokens cover only part of what a Zed theme describes, so each GPUI Kit theme entry
//! may carry the full Zed style under `"zed"`. Entries without it get a Zed theme derived from
//! their GPUI Kit colors. Zed-format files, such as the ones extensions ship, are converted when
//! they load.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex, PoisonError};

use ::settings::Settings as _;
use anyhow::{Context as _, Result};
use gpui::{App, Global, Rgba, SharedString};
use gpui_kit::component::{
    Theme as KitTheme, ThemeMode, ThemeRegistry as KitThemeRegistry, try_parse_color,
};
use gpui_util::ResultExt as _;
use serde_json::{Map, Value, json};
use theme::{Appearance, GlobalTheme, SystemAppearance, ThemeRegistry};

use crate::{
    ThemeAppearanceMode, ThemeFamilyContent, ThemeSelection, ThemeSettings, default_theme,
};

/// GPUI Kit color keys and the Zed style keys they correspond to. Converting a Zed theme fills
/// each GPUI Kit key from its Zed key. Deriving a Zed theme goes the other way, taking the first
/// GPUI Kit key listed for each Zed key.
const COLOR_KEYS: &[(&str, &str)] = &[
    ("background", "background"),
    ("foreground", "text"),
    ("border", "border"),
    ("window.border", "border"),
    ("input.border", "border"),
    ("ring", "border.focused"),
    ("drag.border", "border.focused"),
    ("drop_target.background", "drop_target.background"),
    ("muted.background", "element.background"),
    ("muted.foreground", "text.muted"),
    ("accent.background", "ghost_element.hover"),
    ("accent.foreground", "text"),
    ("primary.background", "text"),
    ("primary.foreground", "background"),
    ("secondary.background", "element.background"),
    ("secondary.hover.background", "element.hover"),
    ("secondary.active.background", "element.active"),
    ("secondary.foreground", "text"),
    ("button.background", "element.background"),
    ("button.hover.background", "element.hover"),
    ("button.active.background", "element.active"),
    ("button.foreground", "text"),
    ("button.primary.background", "text"),
    ("button.primary.foreground", "background"),
    ("danger.background", "error"),
    ("warning.background", "warning"),
    ("success.background", "success"),
    ("info.background", "info"),
    ("link", "text.accent"),
    ("link.hover", "link_text.hover"),
    ("link.active", "text.accent"),
    ("popover.background", "elevated_surface.background"),
    ("popover.foreground", "text"),
    ("list.background", "panel.background"),
    ("list.even.background", "panel.background"),
    ("list.head.background", "panel.background"),
    ("list.hover.background", "ghost_element.hover"),
    ("list.active.background", "ghost_element.selected"),
    ("list.active.border", "border.transparent"),
    ("table.background", "editor.background"),
    ("table.even.background", "editor.background"),
    ("table.head.background", "panel.background"),
    ("table.head.foreground", "text.muted"),
    ("table.hover.background", "ghost_element.hover"),
    ("table.active.background", "ghost_element.selected"),
    ("table.active.border", "border.transparent"),
    ("table.row.border", "border.variant"),
    ("scrollbar.background", "scrollbar.track.background"),
    ("scrollbar.thumb.background", "scrollbar.thumb.background"),
    (
        "scrollbar.thumb.hover.background",
        "scrollbar.thumb.hover_background",
    ),
    ("sidebar.background", "panel.background"),
    ("sidebar.border", "border"),
    ("sidebar.foreground", "text"),
    ("sidebar.accent.background", "ghost_element.hover"),
    ("sidebar.accent.foreground", "text"),
    ("sidebar.primary.background", "ghost_element.selected"),
    ("sidebar.primary.foreground", "text"),
    ("tab.background", "tab.inactive_background"),
    ("tab.active.background", "tab.active_background"),
    ("tab.foreground", "text.muted"),
    ("tab.active.foreground", "text"),
    ("tab_bar.background", "tab_bar.background"),
    ("tab_bar.segmented.background", "element.background"),
    ("title_bar.background", "title_bar.background"),
    ("title_bar.border", "border"),
    ("status_bar.background", "status_bar.background"),
    ("status_bar.border", "border"),
    ("group_box.background", "surface.background"),
    ("group_box.foreground", "text"),
    ("group_box.title.foreground", "text.muted"),
    ("description_list.label.background", "surface.background"),
    ("description_list.label.foreground", "text.muted"),
    ("accordion.background", "surface.background"),
    ("skeleton.background", "element.background"),
    ("switch.background", "text.accent"),
    ("slider.background", "text.accent"),
    ("progress.bar.background", "text.accent"),
    ("chart.1", "terminal.ansi.blue"),
    ("chart.2", "terminal.ansi.green"),
    ("chart.3", "terminal.ansi.yellow"),
    ("chart.4", "terminal.ansi.magenta"),
    ("chart.5", "terminal.ansi.cyan"),
    ("chart.bullish", "terminal.ansi.green"),
    ("chart.bearish", "terminal.ansi.red"),
    ("chart.grid", "border.variant"),
    ("base.red", "terminal.ansi.red"),
    ("base.red.light", "terminal.ansi.bright_red"),
    ("base.green", "terminal.ansi.green"),
    ("base.green.light", "terminal.ansi.bright_green"),
    ("base.yellow", "terminal.ansi.yellow"),
    ("base.yellow.light", "terminal.ansi.bright_yellow"),
    ("base.blue", "terminal.ansi.blue"),
    ("base.blue.light", "terminal.ansi.bright_blue"),
    ("base.magenta", "terminal.ansi.magenta"),
    ("base.magenta.light", "terminal.ansi.bright_magenta"),
    ("base.cyan", "terminal.ansi.cyan"),
    ("base.cyan.light", "terminal.ansi.bright_cyan"),
];

/// Keys of GPUI Kit's `highlight` section, which uses the same names as Zed's style.
const HIGHLIGHT_KEYS: &[&str] = &[
    "editor.background",
    "editor.foreground",
    "editor.active_line.background",
    "editor.line_number",
    "editor.active_line_number",
    "editor.invisible",
    "editor.gutter.background",
    "error",
    "error.background",
    "error.border",
    "warning",
    "warning.background",
    "warning.border",
    "info",
    "info.background",
    "info.border",
    "success",
    "success.background",
    "success.border",
    "hint",
    "hint.background",
    "hint.border",
];

/// GPUI Kit theme entries loaded so far, keyed by theme name. Theme files load on background
/// threads while GPUI Kit's registry lives on the main thread, so entries wait here until
/// [`sync_kit_registry`] rebuilds the registry from them.
#[derive(Default)]
struct KitThemeStore {
    entries: BTreeMap<String, Value>,
    changed: bool,
}

static KIT_THEMES: LazyLock<Mutex<KitThemeStore>> = LazyLock::new(Default::default);

/// Marks an app whose themes GPUI Kit manages; tests and tools that never initialize GPUI Kit
/// keep using Zed's theme registry directly.
struct KitManagedThemes;

impl Global for KitManagedThemes {}

pub(crate) fn is_managed(cx: &App) -> bool {
    cx.has_global::<KitManagedThemes>()
}

pub(crate) fn set_managed(cx: &mut App) {
    cx.set_global(KitManagedThemes);
}

/// Reads a theme file in either format as Zed theme content, along with the GPUI Kit entries it
/// describes.
pub(crate) fn parse_theme_file(bytes: &[u8]) -> Result<(ThemeFamilyContent, Vec<Value>)> {
    let file: Value = serde_json_lenient::from_slice(bytes)?;
    let themes = file
        .get("themes")
        .and_then(Value::as_array)
        .context("theme file has no themes")?;

    // Zed theme entries always have a `style`, and GPUI Kit entries never do.
    let (zed_themes, kit_entries): (Vec<Value>, Vec<Value>) =
        if themes.iter().all(|theme| theme.get("style").is_none()) {
            let zed_themes = themes.iter().filter_map(zed_theme_from_kit_entry).collect();
            (zed_themes, themes.clone())
        } else {
            let kit_entries = themes.iter().filter_map(kit_entry_from_zed_theme).collect();
            (themes.clone(), kit_entries)
        };

    let family = json!({
        "name": file.get("name").and_then(Value::as_str).unwrap_or_default(),
        "author": file.get("author").and_then(Value::as_str).unwrap_or_default(),
        "themes": zed_themes,
    });
    let family: ThemeFamilyContent = serde_json::from_value(family)?;
    Ok((family, kit_entries))
}

pub(crate) fn store_kit_entries(entries: Vec<Value>) {
    let mut store = KIT_THEMES.lock().unwrap_or_else(PoisonError::into_inner);
    for entry in entries {
        if let Some(name) = entry.get("name").and_then(Value::as_str) {
            store.entries.insert(name.to_string(), entry);
            store.changed = true;
        }
    }
}

pub(crate) fn remove_kit_entries(names: &[SharedString]) {
    let mut store = KIT_THEMES.lock().unwrap_or_else(PoisonError::into_inner);
    for name in names {
        if store.entries.remove(name.as_ref()).is_some() {
            store.changed = true;
        }
    }
}

fn kit_entry_from_zed_theme(theme: &Value) -> Option<Value> {
    let name = theme.get("name")?.as_str()?;
    let mode = theme
        .get("appearance")
        .and_then(Value::as_str)
        .unwrap_or("dark");
    let style = theme.get("style")?.as_object()?;

    let mut colors = Map::new();
    for (kit_key, zed_key) in COLOR_KEYS {
        if let Some(value) = style.get(*zed_key).filter(|value| value.is_string()) {
            colors.insert(kit_key.to_string(), value.clone());
        }
    }
    let player = style
        .get("players")
        .and_then(Value::as_array)
        .and_then(|players| players.first());
    for (kit_key, player_key) in [("caret", "cursor"), ("selection.background", "selection")] {
        if let Some(value) = player
            .and_then(|player| player.get(player_key))
            .filter(|value| value.is_string())
        {
            colors.insert(kit_key.to_string(), value.clone());
        }
    }

    let mut highlight = Map::new();
    for key in HIGHLIGHT_KEYS {
        if let Some(value) = style.get(*key).filter(|value| value.is_string()) {
            highlight.insert(key.to_string(), value.clone());
        }
    }
    if let Some(syntax) = style.get("syntax") {
        highlight.insert("syntax".to_string(), syntax.clone());
    }

    Some(json!({
        "name": name,
        "mode": mode,
        "colors": colors,
        "highlight": highlight,
        "zed": style,
    }))
}

fn zed_theme_from_kit_entry(entry: &Value) -> Option<Value> {
    let name = entry.get("name")?.as_str()?;
    let mode = entry.get("mode").and_then(Value::as_str).unwrap_or("light");
    let style = match entry.get("zed").filter(|style| style.is_object()) {
        Some(style) => style.clone(),
        None => derived_zed_style(entry),
    };
    Some(json!({ "name": name, "appearance": mode, "style": style }))
}

fn derived_zed_style(entry: &Value) -> Value {
    let mut style = Map::new();
    if let Some(colors) = entry.get("colors").and_then(Value::as_object) {
        let color = |key: &str| colors.get(key).and_then(Value::as_str).and_then(zed_color);
        for (kit_key, zed_key) in COLOR_KEYS {
            if style.contains_key(*zed_key) {
                continue;
            }
            if let Some(value) = color(kit_key) {
                style.insert(zed_key.to_string(), Value::String(value));
            }
        }
        let mut player = Map::new();
        if let Some(caret) = color("caret") {
            player.insert("cursor".to_string(), Value::String(caret.clone()));
            player.insert("background".to_string(), Value::String(caret));
        }
        if let Some(selection) = color("selection.background") {
            player.insert("selection".to_string(), Value::String(selection));
        }
        if !player.is_empty() {
            style.insert("players".to_string(), json!([player]));
        }
    }
    if let Some(highlight) = entry.get("highlight").and_then(Value::as_object) {
        for (key, value) in highlight {
            style.insert(key.clone(), value.clone());
        }
    }
    Value::Object(style)
}

/// GPUI Kit colors may name a palette color, which Zed cannot read, so they are resolved to hex.
/// Gradients have no single color and are skipped.
fn zed_color(value: &str) -> Option<String> {
    let color = Rgba::from(try_parse_color(value).ok()?);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
    Some(format!(
        "#{:02x}{:02x}{:02x}{:02x}",
        channel(color.r),
        channel(color.g),
        channel(color.b),
        channel(color.a)
    ))
}

/// Rebuilds GPUI Kit's registry from the loaded theme entries. Its registry can only add themes,
/// so a theme file that changed or went away is applied by starting over.
pub(crate) fn sync_kit_registry(cx: &mut App) {
    let entries = {
        let mut store = KIT_THEMES.lock().unwrap_or_else(PoisonError::into_inner);
        if !store.changed {
            return;
        }
        store.changed = false;
        store.entries.values().cloned().collect::<Vec<_>>()
    };

    let mut registry = KitThemeRegistry::default();
    for entry in entries {
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let theme_set = json!({ "name": name, "themes": [entry] });
        registry
            .load_themes_from_str(&theme_set.to_string())
            .with_context(|| format!("loading theme {name:?} into GPUI Kit"))
            .log_err();
    }
    cx.set_global(registry);
}

/// Makes the themes chosen in settings GPUI Kit's active light and dark themes.
pub(crate) fn apply_configured_theme(cx: &mut App) {
    let settings = ThemeSettings::get_global(cx);
    let system_appearance = SystemAppearance::global(cx).0;
    let registry = KitThemeRegistry::global(cx);
    let lookup = |name: &str| {
        registry
            .themes()
            .get(&SharedString::from(name.to_string()))
            .cloned()
    };

    let (light_name, dark_name, mode): (String, String, ThemeMode) = match &settings.theme {
        ThemeSelection::Static(name) => {
            let name = name.0.to_string();
            match lookup(&name).map_or(ThemeMode::Dark, |config| config.mode) {
                ThemeMode::Light => (
                    name,
                    default_theme(Appearance::Dark).to_string(),
                    ThemeMode::Light,
                ),
                ThemeMode::Dark => (
                    default_theme(Appearance::Light).to_string(),
                    name,
                    ThemeMode::Dark,
                ),
            }
        }
        ThemeSelection::Dynamic { mode, light, dark } => {
            let appearance = match mode {
                ThemeAppearanceMode::Light => Appearance::Light,
                ThemeAppearanceMode::Dark => Appearance::Dark,
                ThemeAppearanceMode::System => system_appearance,
            };
            let mode = match appearance {
                Appearance::Light => ThemeMode::Light,
                Appearance::Dark => ThemeMode::Dark,
            };
            (light.0.to_string(), dark.0.to_string(), mode)
        }
    };

    let extensions_loaded = ThemeRegistry::global(cx).extensions_loaded();
    let config = |name: &str, appearance: Appearance| {
        lookup(name).or_else(|| {
            if extensions_loaded {
                log::error!("theme not found: {name}");
            }
            lookup(default_theme(appearance))
        })
    };
    let light = config(&light_name, Appearance::Light);
    let dark = config(&dark_name, Appearance::Dark);

    let theme = KitTheme::global_mut(cx);
    if let Some(light) = light {
        theme.light_theme = light;
    }
    if let Some(dark) = dark {
        theme.dark_theme = dark;
    }
    KitTheme::change(mode, None, cx);
    apply_kit_fonts(cx);
}

/// GPUI Kit components size their text from its theme, so the theme follows the UI and buffer
/// fonts from settings.
pub(crate) fn apply_kit_fonts(cx: &mut App) {
    if !is_managed(cx) {
        return;
    }
    let settings = ThemeSettings::get_global(cx);
    let font_family = settings.ui_font.family.clone();
    let font_size = settings.ui_font_size(cx);
    let mono_font_family = settings.buffer_font.family.clone();
    let mono_font_size = settings.buffer_font_size(cx);

    let theme = KitTheme::global(cx);
    if theme.font_family == font_family
        && theme.font_size == font_size
        && theme.mono_font_family == mono_font_family
        && theme.mono_font_size == mono_font_size
    {
        return;
    }
    KitTheme::update(cx, |theme| {
        theme.font_family = font_family;
        theme.font_size = font_size;
        theme.mono_font_family = mono_font_family;
        theme.mono_font_size = mono_font_size;
    });
}

/// Derives Zed's active theme from GPUI Kit's, so both always show the same theme.
pub(crate) fn sync_zed_theme(cx: &mut App) {
    let name = KitTheme::global(cx).theme_name().clone();
    let theme = match ThemeRegistry::global(cx).get(&name) {
        Ok(theme) => theme,
        Err(error) => {
            log::error!("no Zed theme for GPUI Kit theme {name:?}: {error}");
            return;
        }
    };
    let theme = ThemeSettings::get_global(cx).apply_theme_overrides(theme);
    if **GlobalTheme::theme(cx) == *theme {
        return;
    }
    GlobalTheme::update_theme(cx, theme);
    cx.refresh_windows();
}
