use chrono::{DateTime, Utc};
use gpui::{
    AnyElement, AnyView, App, ClickEvent, ElementId, SharedString, Window, div, prelude::*, rems,
};
use gpui_icons::Icon;
use gpui_kit::component::{
    Selectable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
};
use icons::IconName;
use theme::ActiveTheme;
use ui::rems_from_px;

/// How far rows sit in from the sidebar's edges.
pub(crate) const LIST_INSET: f32 = 8.;
/// The space below an expanded group, before the next one.
pub(crate) const GROUP_GAP: f32 = 16.;

// Rem-based metrics scale with the configured UI font size.
const ROW_MIN_HEIGHT: f32 = 30.;
const ROW_PADDING_X: f32 = 6.;
const ROW_GAP: f32 = 6.;
const ROW_RADIUS: f32 = 8.;
const ICON_BOX: f32 = 20.;
const STATUS_DOT: f32 = 6.;

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

impl RenderOnce for SidebarRow {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let clickable = self.on_click.is_some();
        let group = self.group;

        let hover_state = self
            .leading_on_hover
            .as_ref()
            .map(|_| window.use_keyed_state((self.id.clone(), "leading-hover"), cx, |_, _| false));
        let leading = match (self.leading_on_hover, hover_state.as_ref()) {
            (Some(leading_on_hover), Some(hovered)) if *hovered.read(cx) => leading_on_hover,
            _ => self.leading,
        };
        let observe_hover = hover_state.is_some() || self.on_hover.is_some();
        let colors = cx.theme().colors();

        div().w_full().px(rems_from_px(LIST_INSET)).pb_0p5().child(
            h_flex()
                .id(self.id)
                .group(group)
                .relative()
                .w_full()
                .min_w_0()
                .min_h(rems_from_px(ROW_MIN_HEIGHT))
                .px(rems_from_px(ROW_PADDING_X))
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
                            style.bg(if self.selected {
                                colors.ghost_element_selected
                            } else {
                                colors.ghost_element_hover
                            })
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
                .children(self.end.map(|end| div().flex_none().child(end)))
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

impl RenderOnce for StatusDot {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let (name, variant, color) = match self {
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
                colors.text.opacity(0.16),
            ),
        };
        Icon::empty()
            .id("thread-status-icon")
            .path(name.path_with_variant(variant))
            .text_color(color)
            .size(rems_from_px(if self == StatusDot::Running {
                14.
            } else {
                STATUS_DOT
            }))
            .spinning(self == StatusDot::Running)
    }
}

#[track_caller]
pub(crate) fn sidebar_icon(icon: IconName) -> Icon {
    Icon::empty().path(icon.path()).size(rems(0.875))
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
