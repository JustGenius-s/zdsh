use std::{path::Path, sync::Arc};

use chrono::{DateTime, Utc};
use file_icons::FileIcons;
use gpui::{
    AnyElement, AnyView, App, AvailableSpace, Bounds, ClickEvent, Context, ElementId, Hsla, Role,
    SharedString, TextAlign, Window, canvas, div, point, prelude::*, px, rems, size,
};
use gpui_icons::Icon;
use gpui_kit::component::{
    Icon as KitIcon, Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu},
};
use gpui_kit::img;
use icons::IconName;
use theme::{ActiveTheme, UiTextSize, UiTextTone, UiTypography};
use ui::rems_from_px;
use util::ResultExt as _;

/// How far rows sit in from the sidebar's edges.
pub(crate) const LIST_INSET: f32 = 8.;
/// The space below an expanded group, before the next one.
pub(crate) const GROUP_GAP: f32 = 16.;
/// The space between rows.
pub(crate) const ROW_STACK_GAP: f32 = 2.;

// Rem-based metrics scale with the configured UI font size.
pub(crate) const ROW_MIN_HEIGHT: f32 = 32.;
const ROW_PADDING_X: f32 = 6.;
const ROW_GAP: f32 = 6.;
const ROW_RADIUS: f32 = 8.;
const ICON_BOX: f32 = 20.;
// These glyphs are sized to Cursor's own measurements, so they fill their box as in Cursor's font
// instead of drawing at `gpui_icons::BOX_GLYPH_SCALE`. They take up about 23 of its 24 units:
// 15.2 shows Cursor's 14.5, and 6.4 shows a 6px dot.
const STATUS_DOT: f32 = 6.4;
const GLYPH_SIZE: f32 = 15.2;
// Theme file icons fill most of a 16-unit grid, so 16 shows them about as large as the glyphs.
const FILE_ICON_SIZE: f32 = 16.;

type RowContextMenu = dyn Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu;

/// A row of the threads sidebar: an icon, a title, and something at the end, such as how long
/// ago the thread was updated or buttons that act on it.
#[derive(IntoElement)]
pub(crate) struct SidebarRow {
    id: ElementId,
    group: SharedString,
    leading: AnyElement,
    leading_on_hover: Option<AnyElement>,
    title: AnyElement,
    end: Option<AnyElement>,
    actions_on_hover: Option<AnyElement>,
    end_padding: f32,
    indent: usize,
    context_menu: Option<Box<RowContextMenu>>,
    selected: bool,
    focused: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    on_hover: Option<Box<dyn Fn(&bool, &mut Window, &mut App)>>,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView>>,
}

impl SidebarRow {
    pub(crate) fn new(
        id: impl Into<ElementId>,
        leading: impl IntoElement,
        title: impl IntoElement,
    ) -> Self {
        let id = id.into();
        Self {
            group: SharedString::from(format!("sidebar-row-{id}")),
            id,
            leading: leading.into_any_element(),
            leading_on_hover: None,
            title: title.into_any_element(),
            end: None,
            actions_on_hover: None,
            end_padding: 0.,
            indent: 0,
            context_menu: None,
            selected: false,
            focused: false,
            on_click: None,
            on_hover: None,
            tooltip: None,
        }
    }

    /// Names the row's hover group, for children that show only while the row is hovered.
    pub(crate) fn group(mut self, group: impl Into<SharedString>) -> Self {
        self.group = group.into();
        self
    }

    /// Swaps the icon for `leading` while the row is hovered.
    pub(crate) fn leading_on_hover(mut self, leading: impl IntoElement) -> Self {
        self.leading_on_hover = Some(leading.into_any_element());
        self
    }

    pub(crate) fn end(mut self, end: impl IntoElement) -> Self {
        self.end = Some(end.into_any_element());
        self
    }

    pub(crate) fn actions_on_hover(mut self, actions: impl IntoElement) -> Self {
        self.actions_on_hover = Some(actions.into_any_element());
        self
    }

    pub(crate) fn end_padding(mut self, padding: f32) -> Self {
        self.end_padding = padding;
        self
    }

    pub(crate) fn indent(mut self, depth: usize) -> Self {
        self.indent = depth;
        self
    }

    pub(crate) fn context_menu(
        mut self,
        menu: impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.context_menu = Some(Box::new(menu));
        self
    }

    pub(crate) fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// Rings the row while the keyboard selection is on it.
    pub(crate) fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    pub(crate) fn on_hover(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    pub(crate) fn tooltip(
        mut self,
        tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.tooltip = Some(Box::new(tooltip));
        self
    }
}

impl gpui_kit::component::Selectable for SidebarRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SidebarRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clickable = self.on_click.is_some();
        let group = self.group;
        let context_menu = self.context_menu;

        let hover_state = self
            .leading_on_hover
            .is_some()
            .then(|| window.use_keyed_state((self.id.clone(), "row-hover"), cx, |_, _| false));
        let leading = match (self.leading_on_hover, hover_state.as_ref()) {
            (Some(leading_on_hover), Some(hovered)) if *hovered.read(cx) => leading_on_hover,
            _ => self.leading,
        };
        let observe_hover = hover_state.is_some() || self.on_hover.is_some();
        let end = self.end;
        let has_end = end.is_some() || self.actions_on_hover.is_some();
        let colors = cx.theme().colors();

        let row = div()
            .id((self.id.clone(), "container"))
            .ui_text(
                UiTextSize::Body,
                if self.selected || self.focused {
                    UiTextTone::Primary
                } else {
                    UiTextTone::Secondary
                },
                cx,
            )
            .w_full()
            .px(rems_from_px(LIST_INSET))
            .pb(rems_from_px(ROW_STACK_GAP))
            .child(
                h_flex()
                    .id(self.id)
                    .group(group.clone())
                    .relative()
                    .w_full()
                    .min_w_0()
                    .min_h(rems_from_px(ROW_MIN_HEIGHT))
                    .px(rems_from_px(ROW_PADDING_X))
                    .pl(rems_from_px(ROW_PADDING_X + self.indent as f32 * 14.))
                    .pr(rems_from_px(ROW_PADDING_X + self.end_padding))
                    .gap(rems_from_px(ROW_GAP))
                    .rounded(rems_from_px(ROW_RADIUS))
                    .border_1()
                    .border_color(if self.focused {
                        colors.panel_focused_border
                    } else {
                        gpui::transparent_black()
                    })
                    .when(self.selected, |this| this.bg(colors.ghost_element_selected))
                    .when(clickable, |this| {
                        this.cursor_pointer()
                            .hover(|style| {
                                style.text_color(UiTextTone::Primary.color(cx)).bg(
                                    if self.selected {
                                        colors.ghost_element_selected
                                    } else {
                                        colors.ghost_element_hover
                                    },
                                )
                            })
                            .active(|style| {
                                style.bg(if self.selected {
                                    colors.ghost_element_selected
                                } else {
                                    colors.ghost_element_active
                                })
                            })
                    })
                    .child(
                        div()
                            .size(rems_from_px(ICON_BOX))
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(leading),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .child(self.title),
                    )
                    .when(has_end, |row| {
                        row.child(
                            h_flex()
                                .flex_none()
                                .gap(rems_from_px(6_f32))
                                .children(end)
                                .children(self.actions_on_hover.map(|actions| {
                                    h_flex()
                                        .flex_none()
                                        .invisible()
                                        .group_hover(group, |style| style.visible())
                                        .child(actions)
                                })),
                        )
                    })
                    .when(observe_hover, |this| {
                        this.on_hover(move |hovered, window, cx| {
                            if let Some(state) = &hover_state {
                                state.update(cx, |state, cx| {
                                    if *state != *hovered {
                                        *state = *hovered;
                                        cx.notify();
                                    }
                                });
                            }
                            if let Some(on_hover) = &self.on_hover {
                                on_hover(hovered, window, cx);
                            }
                        })
                    })
                    .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
                    .when_some(self.on_click, |this, on_click| this.on_click(on_click)),
            );
        if let Some(menu) = context_menu {
            row.context_menu(menu).into_any_element()
        } else {
            row.into_any_element()
        }
    }
}

pub(crate) fn fading_label(text: impl Into<SharedString>) -> impl IntoElement {
    FadingLabel(text.into())
}

#[derive(IntoElement)]
struct FadingLabel(SharedString);

impl RenderOnce for FadingLabel {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let text = if self.0.contains(['\n', '\r']) {
            SharedString::from(self.0.replace(['\n', '\r'], " "))
        } else {
            self.0
        };
        let line_height = window.text_style().line_height_in_pixels(window.rem_size());
        div()
            .id("fading-label")
            .role(Role::Label)
            .aria_label(text.clone())
            .w_full()
            .min_w_0()
            .overflow_hidden()
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let text_style = window.text_style();
                        let font_size = text_style.font_size.to_pixels(window.rem_size());
                        let run = text_style.to_run(text.len());
                        let line = Arc::new(window.text_system().shape_line(
                            text.clone(),
                            font_size,
                            &[run],
                            None,
                        ));
                        let mut bands = vec![(bounds, 1.)];

                        // Clip alpha bands of the glyphs themselves: a background overlay would
                        // leave an opaque patch on translucent sidebar chrome.
                        if line.width() > bounds.size.width && bounds.size.width > px(0.) {
                            let scale = window.scale_factor();
                            let fade_width = rems_from_px(16_f32)
                                .to_pixels(window.rem_size())
                                .min(bounds.size.width);
                            let fade_start = px((f32::from(bounds.right() - fade_width) * scale)
                                .round()
                                / scale);
                            bands = vec![(
                                Bounds::new(
                                    bounds.origin,
                                    size(
                                        (fade_start - bounds.left()).max(px(0.)),
                                        bounds.size.height,
                                    ),
                                ),
                                1.,
                            )];
                            let band_width = px(1. / scale);
                            let band_count =
                                (f32::from(bounds.right() - fade_start) * scale).ceil() as usize;
                            for index in 0..band_count {
                                let left = fade_start + band_width * index as f32;
                                let width = band_width.min(bounds.right() - left);
                                if width <= px(0.) {
                                    break;
                                }
                                let opacity = (1.
                                    - ((left - fade_start) + width / 2.) / fade_width)
                                    .clamp(0., 1.);
                                bands.push((
                                    Bounds::new(
                                        point(left, bounds.top()),
                                        size(width, bounds.size.height),
                                    ),
                                    opacity,
                                ));
                            }
                        }
                        let mut element = div()
                            .relative()
                            .w(bounds.size.width)
                            .h(bounds.size.height)
                            .children(bands.into_iter().map(|(clip, opacity)| {
                                let line = line.clone();
                                div()
                                    .absolute()
                                    .left(clip.left() - bounds.left())
                                    .top_0()
                                    .w(clip.size.width)
                                    .h_full()
                                    .overflow_hidden()
                                    .opacity(opacity)
                                    .child(
                                        canvas(
                                            |_, _, _| (),
                                            move |_, _, window, cx| {
                                                line.paint(
                                                    bounds.origin,
                                                    line_height,
                                                    TextAlign::Left,
                                                    None,
                                                    window,
                                                    cx,
                                                )
                                                .log_err();
                                            },
                                        )
                                        .size_full(),
                                    )
                            }))
                            .into_any_element();
                        element.prepaint_as_root(
                            bounds.origin,
                            bounds.size.map(AvailableSpace::Definite),
                            window,
                            cx,
                        );
                        element
                    },
                    |_, mut element, window, cx| element.paint(window, cx),
                )
                .w_full()
                .h(line_height),
            )
    }
}

/// What the dot in front of a thread says about it.
#[derive(IntoElement, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StatusDot {
    Running,
    WaitingForConfirmation,
    Error,
    Unread,
    Draft,
    Read,
}

impl StatusDot {
    fn glyph(self, cx: &App) -> (gpui_icons::IconName, gpui_icons::Variant, Hsla) {
        let colors = cx.theme().colors();
        match self {
            StatusDot::Running => (
                gpui_icons::IconName::Loading,
                gpui_icons::Variant::Outline,
                colors.text_muted,
            ),
            StatusDot::WaitingForConfirmation => (
                gpui_icons::IconName::Circle,
                gpui_icons::Variant::Filled,
                cx.theme().status().warning,
            ),
            StatusDot::Error => (
                gpui_icons::IconName::Circle,
                gpui_icons::Variant::Filled,
                cx.theme().status().error,
            ),
            StatusDot::Unread => (
                gpui_icons::IconName::Circle,
                gpui_icons::Variant::Filled,
                colors.text_accent,
            ),
            StatusDot::Draft => (
                gpui_icons::IconName::Circle,
                gpui_icons::Variant::Outline,
                colors.text.opacity(0.28),
            ),
            StatusDot::Read => (
                gpui_icons::IconName::Circle,
                gpui_icons::Variant::Filled,
                colors.text.opacity(0.24),
            ),
        }
    }

    /// The status as a menu item's icon, as large and in the color the sidebar draws it.
    pub(crate) fn menu_icon(self, cx: &App) -> KitIcon {
        let (name, variant, color) = self.glyph(cx);
        workspace::menu_icon_in_box(
            KitIcon::empty().path(name.path_with_variant(variant)),
            self.size(),
        )
        .text_color(color)
    }

    fn size(self) -> f32 {
        if self == StatusDot::Running {
            GLYPH_SIZE
        } else {
            STATUS_DOT
        }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (name, variant, color) = self.glyph(cx);
        Icon::empty()
            .id("thread-status-icon")
            .path(name.path_with_variant(variant))
            .text_color(color)
            .size(rems_from_px(self.size()))
            .glyph_scale(1.)
            .spinning(self == StatusDot::Running)
    }
}

#[track_caller]
pub(crate) fn sidebar_icon(icon: IconName) -> Icon {
    Icon::empty().path(icon.path()).size(rems(0.875))
}

/// The icon theme's icon for the file at `path`, at the sidebar's icon size.
pub(crate) fn file_icon(path: &Path, cx: &App) -> AnyElement {
    let icon_path = FileIcons::get_icon(path, cx).unwrap_or_else(|| IconName::File.path().into());
    if icon_path.starts_with("icons/")
        || icon_path.starts_with("gpui-icons/")
        || icon_path.starts_with("kit/")
    {
        KitIcon::empty()
            .path(icon_path)
            .size(rems_from_px(FILE_ICON_SIZE))
            .flex_none()
            .text_color(cx.theme().colors().icon_muted)
            .into_any_element()
    } else {
        // Extension themes return disk paths and may contain multicolor SVGs or raster images.
        img(Arc::<Path>::from(Path::new(icon_path.as_ref())))
            .size(rems_from_px(FILE_ICON_SIZE))
            .flex_none()
            .into_any_element()
    }
}

/// An icon from the set Cursor's sidebar draws with, at the sidebar's icon size.
#[track_caller]
pub(crate) fn glyph(name: gpui_icons::IconName) -> Icon {
    Icon::new(name)
        .size(rems_from_px(GLYPH_SIZE))
        .glyph_scale(1.)
}

pub(crate) fn glyph_button(
    id: impl Into<ElementId>,
    name: gpui_icons::IconName,
    cx: &App,
) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .size_6()
        .p_0()
        .flex_none()
        .child(
            glyph(name)
                .id("button-icon")
                .text_color(cx.theme().colors().icon_muted),
        )
}

pub(crate) fn sidebar_icon_button(id: impl Into<ElementId>, icon: IconName) -> Button {
    Button::new(id)
        .ghost()
        .small()
        .size_6()
        .p_0()
        .child(sidebar_icon(icon).id("button-icon").size(rems(1.125)))
}

// Project menus share handles with keyboard navigation and sticky headers.
#[derive(IntoElement)]
pub(crate) struct MenuTrigger(pub Button);

impl ui::Clickable for MenuTrigger {
    fn on_click(self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        Self(self.0.on_click(move |event, window, cx| {
            cx.stop_propagation();
            handler(event, window, cx);
        }))
    }

    fn cursor_style(self, cursor_style: gpui::CursorStyle) -> Self {
        Self(self.0.cursor(cursor_style))
    }
}

impl ui::Toggleable for MenuTrigger {
    fn toggle_state(self, selected: bool) -> Self {
        Self(self.0.selected(selected))
    }
}

impl RenderOnce for MenuTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.0
    }
}

/// How long ago `time` was, as briefly as Cursor's sidebar puts it: "now", "25m", "1h", "7d",
/// "2mo" or "1y".
pub fn relative_time(time: DateTime<Utc>) -> String {
    let seconds = Utc::now().signed_duration_since(time).num_seconds().max(0);
    const MINUTE: i64 = 60;
    const HOUR: i64 = 60 * MINUTE;
    const DAY: i64 = 24 * HOUR;
    const MONTH: i64 = 30 * DAY;
    const YEAR: i64 = 365 * DAY;
    match seconds {
        seconds if seconds < MINUTE => "now".to_string(),
        seconds if seconds < HOUR => format!("{}m", seconds / MINUTE),
        seconds if seconds < DAY => format!("{}h", seconds / HOUR),
        seconds if seconds < MONTH => format!("{}d", seconds / DAY),
        seconds if seconds < YEAR => format!("{}mo", seconds / MONTH),
        seconds => format!("{}y", seconds / YEAR),
    }
}
