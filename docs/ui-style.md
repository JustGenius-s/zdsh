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

Keep layout dimensions separate from typography. Icons use the existing `gpui_icons` helpers and their optical sizing; a 20px slot does not mean a 20px drawing. Sidebar action buttons use `glyph_button`: a 24px control with the same 15.2px glyph and muted icon color as other sidebar controls. Do not inherit a button's default primary foreground for its icon. Preserve the same leading column for group rows and their content unless the content is actually nested.

Navigation selection follows the active tab in the active pane, including changes made through tabs, keyboard navigation, and pane focus. Resolve a thread from its conversation tab and a file from its project path; do not use a panel's last selected thread as the active-tab state. If a file appears in both Files and Changes, both rows represent that active file.

File icons come from `FileIcons` through the shared sidebar helper. Embedded assets use an icon element; extension themes return disk paths, which must load as images so SVG colors and raster formats are preserved. Do not send external theme paths to the embedded asset loader.

Selected rows use the theme's selected background; hovering a selected row keeps that background. Keyboard focus remains visible. Reserve width for trailing metadata only. Hover actions are an absolute overlay at the title lane's right edge, to the **left** of the timestamp or other metadata. They reserve no title space while hidden. On hover, shorten the title's fade boundary to clear the visible controls; the timestamp stays in the same position and never gets covered. Muted metadata remains muted when its row is selected. Keep complete titles and paths available through the existing tooltip or editing flow.

Compute the hover-dependent title inset and button visibility from the same stored row hover state during render, before layout. Do not use `group_hover` to change an anonymous title container's padding: the group may resolve only after layout, leaving the text painted underneath otherwise visible buttons. Keep the title's minimum height stable between these states.

Workspace sidebar headers, section labels, file names, thread titles, terminal titles, and branch names use the shared `fading_label` helper instead of single-line ellipsis. Only overflowing text fades, across the last 16px at the default UI scale before the metadata or currently visible actions. The helper paints clipped glyph alpha bands at device-pixel boundaries, preserving font shaping and fallback without modifying GPUI or painting a background overlay on translucent chrome. Full thread titles remain available in hover tooltips and accessibility labels. Hover actions coexist with metadata: Pin for files, threads and terminals, Archive for saved threads, and Open Source File for changed files. Drafts keep their existing lifecycle and are not discarded through an archive button.

Thread states use the sidebar's `StatusDot`, including the status filter's static previews. Use the sidebar-specific Cursor state mapping rather than its larger task-status icons:

| State | Presentation |
| --- | --- |
| Working | 3×3 sine dot grid, 8 frames at 175ms each; 10.8px optical size at the default UI scale |
| Needs attention | Filled 6.4px dot; dark `#F1B467`, light `#A46700` |
| Error | Filled 6.4px dot; dark `#E34671`, light `#CE405B` |
| Completed, unread | Filled 6.4px dot; dark `#81A1C1`, light `#2778C1`; 88% opacity at rest and full opacity on hover or selection |
| Completed, read | Filled 6.4px dot in the quaternary foreground |
| Draft | Transparent 6.4px circle with a 1px quaternary border |

Quaternary means the current foreground at 28% opacity in dark appearance or 52% in light appearance. These are role-based implementation values; the geometry follows the sidebar's existing optical scale. The grid uses GPUI's shared animation clock, honors reduced motion, and requests only one update per 175ms step. Status filter menus show the first grid frame. Keep these application controls outside GPUI's renderer.

Pinned content appears once, in the first **Pinned** group, before Threads. File and terminal pins share the tabs' existing pin state and workspace persistence. Thread pins persist by project paths and host identity, in pin order. Pinning must not activate a different tab. Unpinning returns an item to its original category. Customize Sidebar and Search have an 8px gap at the default UI scale. **Customize Sidebar** › **Show** › **Archived** toggles an Archived group; opening an archived thread restores it.

Changes shows every changed file across the project's repositories, merged with conversation changes and deduplicated by project path. Git status and statistics take precedence when both sources describe the same file. Missing statistics must not be represented as zero changes. Its toolbar switches between a flat list and a directory tree; only tree directories collapse. Tree content starts one 14px level inside the section header, including root-level files and worktree roots; each nested directory or file adds another 14px. Flat lists retain the ordinary row alignment. The trailing row has an extra 8px inset, and line counts remain visible beside the hover source action. Reuse `change_colors` in `workspace_nav.rs` for counts and Git status rather than the inherited bright created/deleted palette:

| Role | Dark | Light |
| --- | --- | --- |
| Added lines / added files | `#70B489` | `#007041` |
| Removed lines / deleted files | `#FC6B83` | `#BE1744` |
| Modified files | `#F1B467` | `#A46700` |
| Untracked files | `#88C0D0` | `#176C74` |

Conflicts retain the theme's conflict role. These colors apply to Changes metadata; ordinary file titles retain the shared text hierarchy.

The footer has no top divider or changed-file count. Its branch trigger is a GPUI Kit button with the same 32px height, 20px icon slot, 15.2px glyph and text roles as sidebar rows. It opens a GPUI Kit searchable list, with current-branch checks on the right and local/remote filters in a styled Kit menu. Reuse the existing branch delegate for fuzzy search, branch switching, creation, deletion and error prompts; do not embed the inherited picker UI. The popup uses the same body typography and 32px rows, with a 280px default width and 236px minimum.

The left navigation and right file panel resize from their inner edges using GPUI Kit resize handles. Handles never draw a full-height divider. Keep the hit areas invisible at rest; hover, press and drag show only a short, rounded 2px grip, centered vertically and inset inside the panel. Paint this grip in the ordinary clipped layer, without deferring it beyond the panel or overlapping the content block's rounded border. Their default widths are 272px and 264px; user widths range from 200px to 600px at the default UI scale. Each panel is also limited to half the window width remaining after a 240px center and the outer inset; narrow windows may temporarily display them below the normal minimum. Store user widths in UI-scale-independent units per project paths and host when a drag ends, including a release outside the panel. Window resizing only clamps the displayed size and must not overwrite the preference. Floating and pinned file panels share one width, and persistence errors must reach the user.

The Changes section has its own action to open the Changes panel; clicking a changed file opens a single-file diff, and its hover action opens the source file. Keep these actions separate, and stop propagation from row action buttons so they do not activate or fold the containing row.

Keep title-bar mouse-event barriers scoped to the actual project pill track and individual controls. Flexible spacers and centered gutters must bubble mouse events to GPUI Kit's `TitleBar`, which owns window dragging and invokes the platform's native double-click action. In particular, an expanding container around centered project controls must remain draggable outside those controls.

Keep the title-bar region from shrinking. The workspace body below it fills the remaining height with `flex_1`, not `h_full` or `size_full`; give the body and its flexible content containers zero minimum width and height so their contents cannot push them beyond the window. The rounded content block keeps a 4px inset from both the right and bottom window edges at the default UI scale. Center panes, including zoomed panes, fill the width left by docks and centered-layout padding with `flex_1` and `min_w_0`, rather than claiming the entire row's width. Scrolling belongs to the lists and content views inside this bounded region.

Enable the welcome-page fallback on every workspace center pane, both the initial pane and panes created by `Workspace::add_pane` during layout restoration or splitting. Embedded debugger and diff panes keep their separate defaults. The fallback renders when a center pane has no active item and the project has no visible worktrees; restoring a layout must not lose this capability. Changing only the startup entry point cannot make a restored pane display the start page.

## Menus

Pass every GPUI Kit menu through `workspace::style_menu`, and every submenu through `workspace::style_submenu`. They own the 236px / 200px minimum widths and trailing check marks.

Use `workspace::menu_icon`, `menu_icon_spacer`, and `menu_heading` for aligned icon menus. Keep content-filled lists and menus that begin with a block of text iconless as described in `AGENTS.md`. Preserve GPUI Kit's keyboard, focus, disabled-state, and accessibility behavior.

Thread context menus expose Pin / Unpin, Rename, Mark as Read / Unread, Fork, Copy, and Archive (Restore for archived threads). Copy offers the title, thread ID, and conversation. Disable unsupported operations: forks require a saved, idle native conversation, and closed external conversations cannot export content through the native store. Rename uses an inline input; Enter or blur commits a nonempty title and Escape cancels.

## Review a UI change

The user performs visual verification: check the normal, selected, hovered, keyboard-focused, empty, loading, and search-result states relevant to the change. Compare at the same window scale and width, including long Latin text and Chinese text, and inspect both light and dark themes. Check a larger UI font setting so fixed trailing controls cannot squeeze the title out of the row. Agents must not run UI tests, launch the app for visual verification, or use Computer Use to operate the interface.

Confirm the text-owning component uses a shared role, its line height is explicit, and semantic status colors remain intact. Run `cargo check --locked -p zed --bin zed`; use `./script/clippy` for Clippy. A successful compile checks implementation, not visual fidelity.

## Reference and implementation choices

The baseline follows the Cursor 3.27.6 sidebar investigation from 2026-10-24. Cursor's package defines 13px / 18px body text; the inspected local `window.zoomLevel: 0.4` yields approximately 14px / 19.4px. zdsh uses 14px / 20px as an explicit application standard while preserving the user's own scaling. This is a design choice based on that reference, not a claim that Cursor's unscaled token is 14px.

The rasterization comparison also checked Cursor's shipped smoothing setting and CSS against [Chromium's macOS font policy](https://chromium.googlesource.com/chromium/src/+/1d56cda2d64d8479287a5eaf675e508ebce7d50d/third_party/blink/renderer/platform/fonts/mac/font_platform_data_mac.mm#205). Chromium's `antialiased` policy keeps antialiasing enabled while disabling font smoothing. GPUI's corresponding controls are in `crates/gpui/src/window.rs` and `crates/gpui_apple/src/text_system.rs`; this does not imply that Cursor's exact bundled Chromium revision was rebuilt or tested.

The primary / secondary / muted hierarchy and the row geometry were checked against shipped styles and the supplied screenshots. REA Evidence: `ev_c8cbcfe1c5d5784ef09c915cefdee7dee62a1d7c2c15cc464dfa9fc6aabe0d74`; selected-carrier application coverage was complete, semantic coverage remained unknown. The analysis did not execute Cursor or establish runtime computed styles. See [the earlier layout research](cursor-thread-layout-research.md) for related package evidence; this standard supersedes its choice to leave sidebars at the inherited UI body size.

The Git palette above comes from the dark and light `gitDecoration.*ResourceForeground` definitions in Cursor's shipped `extensions/theme-cursor` package, inspected on 2026-10-24 with REA Evidence `ev_1283fdc51fb9843caa4f65a53866bcadfd70df97a3826a08b03607a8e491ba20` (subject SHA-256 `2c5919f6ab870d93e3b1a9cb7f5212d510ae792823cabafb83768db09d95bd6f`). This evidence covers the theme package only. Separate static inspection of the shipped workbench found added/deleted counts using those Git roles, an Archived visibility flag, and Pinned/Archived group labels. Hover spacing, mixed pinned content, and the tree layout are zdsh implementation choices based on the supplied references; no runtime or pixel equivalence is claimed.

Thread states were checked again against Cursor **3.27.12** on 2026-10-24. REA Evidence `ev_56cac9a2b52dd1f0fa0b7973195f37af0b7c1c10d560d0d04b11ee90f424e9b9` covers four inert carriers extracted from `workbench.desktop.main.js`: `AgentStatusDot`, `agent-leading-status`, the sidebar leading renderer, and dot-grid definitions. All four parsed without errors; carrier SHA-256 is `6a16358b901c9da2d9fd4f0870d3720ddea67291aae0280a465a627f1520eac0`. Source byte ranges, hashes and complete Evidence are saved under `/tmp/zdsh-thread-status-20261024/`. External bindings remain unresolved and this is not whole-application or runtime coverage. Separate static inspection of the shipped CSS and theme JSON resolved yellow/red/accent roles to the palette above and found the 88% subdued-unread rule. The quaternary values are taken from Cursor's foreground roles; the exact computed cascade of its done-seen atom was not established. Button placement follows the user's explicit overlay requirement. No Cursor or zdsh UI was launched or tested.
