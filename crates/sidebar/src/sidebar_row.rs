use std::{path::Path, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use file_icons::FileIcons;
use gpui::{
    Animation, AnimationExt as _, AnyElement, AnyView, App, AvailableSpace, Bounds, ClickEvent,
    Context, ElementId, Hsla, Role, SharedString, TextAlign, Window, canvas, div, point,
    prelude::*, px, rems, rgb, size,
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
use theme::{ActiveTheme, Appearance, UiTextSize, UiTextTone, UiTypography};
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
    actions_on_hover: Option<(AnyElement, f32)>,
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

    pub(crate) fn actions_on_hover(
        mut self,
        actions: impl IntoElement,
        button_count: usize,
    ) -> Self {
        if button_count > 0 {
            let width = button_count as f32 * 24. + button_count.saturating_sub(1) as f32 * 2.;
            self.actions_on_hover = Some((actions.into_any_element(), width));
        }
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

        let hover_state = (self.leading_on_hover.is_some() || self.actions_on_hover.is_some())
            .then(|| window.use_keyed_state((self.id.clone(), "row-hover"), cx, |_, _| false));
        let hovered = hover_state.as_ref().is_some_and(|state| *state.read(cx));
        let leading = match (self.leading_on_hover, hover_state.as_ref()) {
            (Some(leading_on_hover), Some(hovered)) if *hovered.read(cx) => leading_on_hover,
            _ => self.leading,
        };
        let observe_hover = hover_state.is_some() || self.on_hover.is_some();
        let end = self.end;
        let has_end = end.is_some();
        let actions = self.actions_on_hover.filter(|_| hovered);
        let action_width = actions.as_ref().map(|(_, width)| *width);
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
                            .min_h(rems_from_px(24_f32))
                            .relative()
                            .overflow_hidden()
                            .when_some(action_width, |title, width| {
                                // Anonymous group-hover styles resolve after layout. Compute the
                                // title's inset from row state so its fade clears the buttons.
                                title.pr(rems_from_px(width + ROW_GAP))
                            })
                            .child(self.title)
                            .when_some(actions, |title, (actions, _)| {
                                title.child(
                                    h_flex()
                                        .absolute()
                                        .right_0()
                                        .top_0()
                                        .bottom_0()
                                        .child(actions),
                                )
                            }),
                    )
                    .when(has_end, |row| {
                        row.child(h_flex().flex_none().gap(rems_from_px(6_f32)).children(end))
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
    fn quaternary(cx: &App) -> Hsla {
        cx.theme()
            .colors()
            .text
            .opacity(match cx.theme().appearance() {
                Appearance::Dark => 0.28,
                Appearance::Light => 0.52,
            })
    }

    pub(crate) fn color(self, cx: &App) -> Hsla {
        let (warning, error, accent) = match cx.theme().appearance() {
            Appearance::Dark => (0xf1b467, 0xe34671, 0x81a1c1),
            Appearance::Light => (0xa46700, 0xce405b, 0x2778c1),
        };
        match self {
            Self::Running => UiTextTone::Secondary.color(cx),
            Self::WaitingForConfirmation => rgb(warning).into(),
            Self::Error => rgb(error).into(),
            Self::Unread => rgb(accent).into(),
            Self::Draft | Self::Read => Self::quaternary(cx),
        }
    }

    /// The status as a menu item's icon, as large and in the color the sidebar draws it.
    pub(crate) fn menu_icon(self, cx: &App) -> KitIcon {
        let data: &[u8] = match self {
            Self::Running => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10.5 10.5"><g fill="currentColor"><circle cx="5.25" cy="1.25" r="1.125"/><circle cx="1.25" cy="5.25" r="1.125"/><circle cx="5.25" cy="5.25" r="1.125"/><circle cx="9.25" cy="5.25" r="1.125"/><circle cx="1.25" cy="9.25" r="1.125"/><circle cx="9.25" cy="9.25" r="1.125"/></g></svg>"#,
            Self::Draft => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 6.4 6.4"><circle cx="3.2" cy="3.2" r="2.7" fill="none" stroke="currentColor" stroke-width="1"/></svg>"#,
            _ => br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 6.4 6.4"><circle cx="3.2" cy="3.2" r="3.2" fill="currentColor"/></svg>"#,
        };
        workspace::menu_icon_in_box(KitIcon::empty().data(data), self.size())
            .text_color(self.color(cx))
    }

    fn size(self) -> f32 {
        if self == StatusDot::Running {
            10.8
        } else {
            STATUS_DOT
        }
    }
}

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color(cx);
        if self == Self::Running {
            const FRAMES: [u16; 8] = [189, 220, 90, 78, 45, 291, 306, 433];
            return div()
                .id("thread-status-icon")
                .relative()
                .size(rems_from_px(10.8_f32))
                .role(Role::Label)
                .aria_label("In progress")
                .with_animation(
                    "thread-status-grid",
                    Animation::new(Duration::from_millis(1400))
                        .repeat_synced()
                        .with_max_fps(1000. / 175.),
                    move |grid, delta| {
                        let frame = (delta * FRAMES.len() as f32) as usize;
                        let mask = FRAMES.get(frame).copied().unwrap_or(189);
                        grid.children((0..9).filter_map(move |cell| {
                            (mask & (1 << (8 - cell)) != 0).then(|| {
                                // Cursor's 10.5-unit SVG grid has 2.25-unit dots, 4 units apart.
                                let scale = 10.8 / 10.5;
                                div()
                                    .absolute()
                                    .left(rems_from_px((0.125 + (cell % 3) as f32 * 4.) * scale))
                                    .top(rems_from_px((0.125 + (cell / 3) as f32 * 4.) * scale))
                                    .size(rems_from_px(2.25 * scale))
                                    .rounded_full()
                                    .bg(color)
                            })
                        }))
                    },
                )
                .into_any_element();
        }
        let label = match self {
            Self::WaitingForConfirmation => "Needs attention",
            Self::Error => "Error",
            Self::Unread => "Completed, unread",
            Self::Draft => "Draft",
            Self::Read => "Completed",
            Self::Running => "In progress",
        };
        div()
            .id("thread-status-icon")
            .size(rems_from_px(STATUS_DOT))
            .rounded_full()
            .role(Role::Label)
            .aria_label(label)
            .when(self == Self::Draft, |dot| {
                dot.border_1().border_color(color)
            })
            .when(self != Self::Draft, |dot| dot.bg(color))
            .into_any_element()
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
