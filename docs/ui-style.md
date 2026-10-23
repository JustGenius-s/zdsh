# UI style standard

Use this standard for new or restyled application controls: navigation, sidebars, lists, menus, toolbars, and settings. Editor buffers, terminal contents, and conversation prose keep their own typography. Existing screens migrate when they are changed; this document does not imply that every inherited Zed component already follows it.

Build with GPUI Kit components first, then its base APIs. Shared application styling lives in `theme::{UiTextSize, UiTextTone, UiTypography}`. Extend that implementation when a reusable role is missing rather than adding local font sizes or opacity constants. Do not change `crates/gpui*` to implement application styling.

## Typography and scaling

The default UI font is `.SystemUIFont`. Explicit `ui_font_family`, font features, fallbacks, and weight preferences remain authoritative. Code and terminal font settings are separate.

| Role | Font size | Line height | Use |
| --- | --- | --- | --- |
| `UiTextSize::Body` | 14px | 20px | Navigation, list entries, buttons, group labels, empty states, single-line timestamps |
| `UiTextSize::Caption` | 12px | 16px | Supplementary descriptions and compact metadata |

These are logical pixels at the default 16px UI setting, expressed as rem values. Increasing the user's UI font setting scales both typography and layout. Choosing a smaller text role changes text only: do not reset the window rem size, reduce the global UI font setting, or shrink an entire row to make its label smaller.

Always choose a text size and line height together. An unstyled `div()` inherits 16px text and GPUI's paragraph line height; it is not the default application label. Group labels use the ordinary body size and weight. Selection changes emphasis, not font size or weight.

## Foreground roles

| Role | Resolution | Use |
| --- | --- | --- |
| `UiTextTone::Primary` | Theme `text` | Current, hovered, or keyboard-focused content |
| `UiTextTone::Secondary` | Theme `text` at 74% opacity | Ordinary unselected content |
| `UiTextTone::Muted` | Theme `text_muted` | Group labels, empty states, timestamps, descriptions |

The secondary tier is deliberately distinct from muted: Z themes use 60% opacity for muted text. Do not give every item the primary foreground, or substitute muted for ordinary content. Derive colors from the active theme, not sampled screenshot RGB values. Error, warning, success, Git status, and search-match emphasis retain their semantic colors.

For example:

```rust
use theme::{UiTextSize, UiTextTone, UiTypography};

let tone = if selected {
    UiTextTone::Primary
} else {
    UiTextTone::Secondary
};
let label = gpui_kit::component::label::Label::new(title)
    .ui_text(UiTextSize::Body, tone, cx)
    .truncate();
```

Apply roles at the component that actually owns the text style. GPUI Kit labels can set their own foreground and line height, overriding a styled parent. Fuzzy-search results, renamed titles, loading states, and ordinary entries must retain the same text metrics.

## Text rasterization

On macOS, the default `text_rendering_mode` is `grayscale`, matching the policy selected by Cursor's default `cursor.general.fontSmoothingAntialiased_v2` setting and `-webkit-font-smoothing: antialiased`.

Keep CoreGraphics antialiasing and fractional glyph positioning enabled. Grayscale mode uses zero glyph dilation and explicitly disables CoreGraphics font smoothing so it cannot add luminance-dependent stroke thickening. Do not compensate for a rasterization mismatch by changing font weight, foreground opacity, or font size.

The application applies the setting at startup and whenever settings change. The existing glyph cache key includes dilation, so the two policies use separate glyph images. This is an application-wide rendering policy, including editor and terminal glyphs; their font families, sizes, and line heights remain independently configurable.

Explicit user choices remain authoritative:

| Mode | macOS behavior |
| --- | --- |
| `grayscale` | Antialiasing enabled; no extra CoreGraphics stroke thickening |
| `platform_default` | Existing platform smoothing, including its `AppleFontSmoothing` preference handling |
| `subpixel` | Existing platform behavior; macOS does not support GPUI's LCD subpixel rendering path |

Changing the default does not rewrite existing user settings or change macOS preferences. Windows and Linux retain their existing defaults and rasterization behavior. Keep this policy at the application setting boundary; the GPUI changes are limited to honoring explicit grayscale mode on macOS and disabling smoothing for zero dilation.

Matching this policy does not guarantee identical pixels: font fallback, font variation axes, display scale, fractional glyph positions, and Chromium's composition can still differ. The user performs the visual comparison; agents do not run UI tests or operate the interface.

## Lists and navigation

The shared sidebar row implementation is `crates/sidebar/src/sidebar_row.rs`. Reuse it within sidebars; do not create a second set of row constants in a caller.

| Metric | Value at the default UI scale |
| --- | --- |
| Row minimum height | 32px |
| Outer horizontal inset | 8px |
| Inner horizontal padding | 6px |
| Icon slot | 20px |
| Icon-to-label gap | 6px |
| Row gap | 2px |
| Group gap | 16px |
| Row corner radius | 8px |

Keep layout dimensions separate from typography. Icons use the existing `gpui_icons` helpers and their optical sizing; a 20px slot does not mean a 20px drawing. Preserve the same leading column for group rows and their content unless the content is actually nested.

Selected rows use the theme's selected background; hovering a selected row keeps that background. Keyboard focus remains visible. Reserve space for trailing metadata and actions, and let only the title shrink. Muted metadata remains muted when its row is selected. Keep complete titles and paths available through the existing tooltip or editing flow.

Current sidebar labels use single-line ellipsis. Cursor's agent-title fade is a separate overflow behavior, not a typography setting; migrate it through a shared component when implemented. Do not imitate a transparency mask by painting a flat background-colored gradient over translucent application chrome.

## Menus

Pass every GPUI Kit menu through `workspace::style_menu`, and every submenu through `workspace::style_submenu`. They own the 236px / 200px minimum widths and trailing check marks.

Use `workspace::menu_icon`, `menu_icon_spacer`, and `menu_heading` for aligned icon menus. Keep content-filled lists and menus that begin with a block of text iconless as described in `AGENTS.md`. Preserve GPUI Kit's keyboard, focus, disabled-state, and accessibility behavior.

## Review a UI change

The user performs visual verification: check the normal, selected, hovered, keyboard-focused, empty, loading, and search-result states relevant to the change. Compare at the same window scale and width, including long Latin text and Chinese text, and inspect both light and dark themes. Check a larger UI font setting so fixed trailing controls cannot squeeze the title out of the row. Agents must not run UI tests, launch the app for visual verification, or use Computer Use to operate the interface.

Confirm the text-owning component uses a shared role, its line height is explicit, and semantic status colors remain intact. Run `cargo check --locked -p zed --bin zed`; use `./script/clippy` for Clippy. A successful compile checks implementation, not visual fidelity.

## Reference and implementation choices

The baseline follows the Cursor 3.27.6 sidebar investigation from 2026-10-24. Cursor's package defines 13px / 18px body text; the inspected local `window.zoomLevel: 0.4` yields approximately 14px / 19.4px. zdsh uses 14px / 20px as an explicit application standard while preserving the user's own scaling. This is a design choice based on that reference, not a claim that Cursor's unscaled token is 14px.

The rasterization comparison also checked Cursor's shipped smoothing setting and CSS against [Chromium's macOS font policy](https://chromium.googlesource.com/chromium/src/+/1d56cda2d64d8479287a5eaf675e508ebce7d50d/third_party/blink/renderer/platform/fonts/mac/font_platform_data_mac.mm#205). Chromium's `antialiased` policy keeps antialiasing enabled while disabling font smoothing. GPUI's corresponding controls are in `crates/gpui/src/window.rs` and `crates/gpui_apple/src/text_system.rs`; this does not imply that Cursor's exact bundled Chromium revision was rebuilt or tested.

The primary / secondary / muted hierarchy and the row geometry were checked against shipped styles and the supplied screenshots. REA Evidence: `ev_c8cbcfe1c5d5784ef09c915cefdee7dee62a1d7c2c15cc464dfa9fc6aabe0d74`; selected-carrier application coverage was complete, semantic coverage remained unknown. The analysis did not execute Cursor or establish runtime computed styles. See [the earlier layout research](cursor-thread-layout-research.md) for related package evidence; this standard supersedes its choice to leave sidebars at the inherited UI body size.
