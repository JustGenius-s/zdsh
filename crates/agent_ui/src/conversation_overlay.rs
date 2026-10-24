use crate::{
    AgentPanel, DismissConversationOverlay, ExpandConversationOverlay, ToggleConversationOverlay,
    conversation_item::ConversationItem,
};
use acp_thread::ThreadStatus;
use agent_settings::AgentSettings;
use gpui::{
    Context, ElementId, Entity, EntityId, Focusable, Pixels, Subscription, WeakEntity, Window, div,
    prelude::*, px,
};
use gpui_kit::{
    base::{
        Button,
        animation::Lerp as _,
        h_flex,
        motion::{Easing, MotionStatus, Transition, transition, transition_with_status},
    },
    component::{Theme as KitTheme, kbd::Kbd},
};
use settings::Settings;
use std::{cell::Cell, rc::Rc, time::Duration};
use theme::{UiTextSize, UiTextTone, UiTypography};
use theme_settings::ThemeSettings;
use util::ResultExt;
use workspace::{Event, Workspace};
use zed_actions::assistant::ToggleFocus;

#[derive(Clone, Copy)]
pub(crate) struct OverlayInputShape {
    pub height: Pixels,
    pub corner_radius: Pixels,
}

pub(crate) struct ConversationOverlay {
    workspace: WeakEntity<Workspace>,
    item: Option<WeakEntity<ConversationItem>>,
    expanded: bool,
    available: bool,
    active: bool,
    hovered: bool,
    dismissed: bool,
    attention: bool,
    closing_input_shape: Option<OverlayInputShape>,
    host_width: Rc<Cell<Option<Pixels>>>,
    item_observation: Option<Subscription>,
    item_focus_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
}

impl ConversationOverlay {
    pub(crate) fn new(workspace: &Workspace, cx: &mut Context<Self>) -> Self {
        let mut subscriptions = Vec::new();
        for dock in workspace.all_docks() {
            subscriptions.push(cx.observe(dock, |this, _, cx| this.sync(cx)));
        }
        let workspace = workspace.weak_handle();
        if let Some(workspace) = workspace.upgrade() {
            subscriptions.push(cx.subscribe(&workspace, |this, _, event, cx| match event {
                Event::ActiveItemChanged | Event::PaneRemoved | Event::ZoomChanged => this.sync(cx),
                Event::ItemRemoved { item_id } => {
                    if this
                        .item
                        .as_ref()
                        .is_some_and(|item| item.entity_id() == *item_id)
                    {
                        this.set_item(None, cx);
                    }
                    this.sync(cx);
                }
                _ => {}
            }));
        }
        Self {
            workspace,
            item: None,
            expanded: false,
            available: false,
            active: false,
            hovered: false,
            dismissed: false,
            attention: false,
            closing_input_shape: None,
            host_width: Rc::default(),
            item_observation: None,
            item_focus_subscription: None,
            _subscriptions: subscriptions,
        }
    }

    fn set_item(&mut self, item: Option<Entity<ConversationItem>>, cx: &mut Context<Self>) {
        if self.item.as_ref().map(|item| item.entity_id()) == item.as_ref().map(Entity::entity_id) {
            return;
        }
        if let Some(previous) = self.item.take().and_then(|item| item.upgrade()) {
            previous.update(cx, |item, cx| item.set_overlay_presentation(None, cx));
        }
        self.item_observation = item
            .as_ref()
            .map(|item| cx.observe(item, |this, _, cx| this.sync(cx)));
        self.item_focus_subscription = None;
        // The tab owns the session; hiding the floating input must not retain a closed tab.
        self.item = item.map(|item| item.downgrade());
        self.expanded = false;
        self.active = false;
        self.hovered = false;
        self.dismissed = false;
        self.attention = false;
        self.closing_input_shape = None;
    }

    pub(crate) fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.upgrade() else {
            self.available = false;
            cx.notify();
            return;
        };
        let active_item = workspace.read(cx).active_item(cx);
        if let Some(item) = active_item
            .as_ref()
            .and_then(|item| item.downcast::<ConversationItem>())
        {
            self.set_item(Some(item), cx);
        }
        let item = self.item.as_ref().and_then(WeakEntity::upgrade);
        let was_available = self.available;
        self.available = item.is_some()
            && active_item.is_some()
            && !AgentPanel::is_visible(&workspace, cx)
            && workspace.read(cx).zoomed_item().is_none();
        let attention = item
            .as_ref()
            .is_some_and(|item| item.read(cx).overlay_needs_attention(cx));
        if !self.available {
            self.active = false;
            self.expanded = false;
            self.hovered = false;
        } else if attention && (!self.attention || !was_available) {
            self.active = true;
            self.dismissed = false;
        }
        self.attention = attention;
        if let Some(item) = item {
            // Keep presentation separate from mounting: a dormant input does not render the item.
            let expanded = self.available.then_some(self.expanded);
            item.update(cx, |item, cx| item.set_overlay_presentation(expanded, cx));
        }
        cx.notify();
    }

    pub(crate) fn is_available(&self) -> bool {
        self.available
    }

    pub(crate) fn activate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.available {
            return;
        }
        self.active = true;
        self.hovered = false;
        self.dismissed = false;
        self.sync(cx);
        if let Some(item) = self.item.as_ref().and_then(WeakEntity::upgrade) {
            let focus = item.focus_handle(cx);
            focus.focus(window, cx);
        }
    }

    fn deactivate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let item = self.item.as_ref().and_then(WeakEntity::upgrade);
        self.closing_input_shape = item
            .as_ref()
            .and_then(|item| item.read(cx).overlay_thread(cx))
            .and_then(|thread| thread.read(cx).overlay_input_shape());
        let focused = item
            .as_ref()
            .is_some_and(|item| item.focus_handle(cx).contains_focused(window, cx));
        self.expanded = false;
        self.active = false;
        self.hovered = false;
        self.dismissed = true;
        if focused && let Some(workspace) = self.workspace.upgrade() {
            let focus = workspace.read(cx).active_pane().focus_handle(cx);
            focus.focus(window, cx);
        }
        self.sync(cx);
    }

    pub(crate) fn dismiss(
        &mut self,
        _: &DismissConversationOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.available || !self.active {
            cx.propagate();
        } else if self.expanded {
            self.toggle(&ToggleConversationOverlay, window, cx);
        } else {
            self.deactivate(window, cx);
        }
    }

    pub(crate) fn submitted(
        &mut self,
        thread_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.expanded
            && self
                .item
                .as_ref()
                .and_then(WeakEntity::upgrade)
                .is_some_and(|item| {
                    item.read(cx)
                        .overlay_thread(cx)
                        .is_some_and(|thread| thread.entity_id() == thread_id)
                })
        {
            self.deactivate(window, cx);
        }
    }

    pub(crate) fn toggle(
        &mut self,
        _: &ToggleConversationOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.available {
            cx.propagate();
            return;
        }
        self.active = true;
        self.dismissed = false;
        self.expanded = !self.expanded;
        if !self.expanded
            && let Some(item) = self.item.as_ref().and_then(WeakEntity::upgrade)
            && let Some(focus) = item.read(cx).overlay_composer_focus_handle(window, cx)
        {
            focus.focus(window, cx);
        }
        self.sync(cx);
    }

    pub(crate) fn expand(
        &mut self,
        _: &ExpandConversationOverlay,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.item.as_ref().and_then(WeakEntity::upgrade) else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.activate_item(&item, true, true, window, cx);
                })
                .log_err();
        });
    }

    fn outside_click(
        &mut self,
        _: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.deactivate_when_tab_focused(window, cx);
    }

    fn deactivate_when_tab_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let overlay = cx.weak_entity();
        // Portal menus live outside these bounds. Wait for focus to settle before deciding
        // whether the click returned to the underlying tab or belongs to a popup.
        window.defer(cx, move |window, cx| {
            overlay
                .update(cx, |overlay, cx| {
                    if overlay.active
                        && overlay.workspace.upgrade().is_some_and(|workspace| {
                            workspace
                                .read(cx)
                                .active_pane()
                                .focus_handle(cx)
                                .contains_focused(window, cx)
                        })
                    {
                        overlay.deactivate(window, cx);
                    }
                })
                .log_err();
        });
    }
}

impl Render for ConversationOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = ThemeSettings::get_global(cx).agent_ui_font_size(cx);
        let available_width = (self
            .host_width
            .get()
            .unwrap_or(window.viewport_size().width)
            - font_size * 2.)
            .max(px(0.));
        let width = AgentSettings::get_global(cx)
            .max_content_width
            .map_or(font_size * 38., |maximum| maximum.min(font_size * 38.))
            .min(available_width);
        let host_width = self.host_width.clone();
        let view_id = cx.entity_id();
        let item = self
            .available
            .then(|| self.item.as_ref()?.upgrade())
            .flatten();
        div()
            .id("conversation-overlay")
            .absolute()
            .bottom_4()
            .left_0()
            .w_full()
            .h_0()
            .flex()
            .justify_center()
            .on_action(cx.listener(Self::toggle))
            .on_action(cx.listener(Self::expand))
            .on_action(cx.listener(Self::dismiss))
            .child(
                gpui::canvas(
                    move |bounds, _, cx| {
                        if host_width.replace(Some(bounds.size.width)) != Some(bounds.size.width) {
                            cx.notify(view_id);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(item.map(|item| {
                if self.item_focus_subscription.is_none() {
                    self.item_focus_subscription = Some(cx.on_focus_out(
                        &item.focus_handle(cx),
                        window,
                        |this, _, window, cx| this.deactivate_when_tab_focused(window, cx),
                    ));
                }
                let status = if self.attention {
                    Some("Needs attention".into())
                } else {
                    item.read(cx).overlay_thread(cx).and_then(|thread| {
                        (thread.read(cx).thread.read(cx).status() == ThreadStatus::Generating)
                            .then(|| thread.read(cx).overlay_status_label(cx))
                    })
                };
                let motion =
                    Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                        x1: 0.215,
                        y1: 0.61,
                        x2: 0.355,
                        y2: 1.,
                    });
                let activation = transition(
                    ElementId::from(("conversation-overlay-activation", cx.entity_id())),
                    if self.active { 1. } else { 0. },
                    motion.clone(),
                    window,
                    cx,
                );
                // Cursor animates measured height separately from the CSS width transition.
                // Seed at one while mounted, including during an interrupted expansion.
                let closing_height = transition(
                    ElementId::from(("conversation-overlay-closing-height", cx.entity_id())),
                    if self.active { 1. } else { 0. },
                    if self.active {
                        Transition::new(Duration::ZERO)
                    } else {
                        Transition::new(Duration::from_millis(150)).easing(Easing::CubicBezier {
                            x1: 0.16,
                            y1: 1.,
                            x2: 0.3,
                            y2: 1.,
                        })
                    },
                    window,
                    cx,
                );
                let returning_to_handle = !self.active && (activation > 0. || closing_height > 0.);
                let show_label = !self.active
                    && !returning_to_handle
                    && (self.hovered || (!self.dismissed && status.is_some()));
                // Active and dismissed inputs must not leave a hover capsule behind.
                let reveal = transition(
                    ElementId::from(("conversation-overlay-reveal", cx.entity_id())),
                    if show_label { 1. } else { 0. },
                    if self.active || returning_to_handle {
                        Transition::new(Duration::ZERO)
                    } else {
                        motion.clone()
                    },
                    window,
                    cx,
                );
                let animated_width = transition_with_status(
                    ElementId::from(("conversation-overlay-width", cx.entity_id())),
                    if self.active {
                        width
                    } else {
                        (font_size * if show_label { 14. } else { 8. }).min(available_width)
                    },
                    motion,
                    window,
                    cx,
                );
                let width_animating = self.active
                    && matches!(
                        animated_width.status,
                        MotionStatus::Running | MotionStatus::Delayed
                    );
                if let Some(thread) = item.read(cx).overlay_thread(cx) {
                    thread.update(cx, |thread, cx| {
                        thread.set_overlay_width_animating(width_animating, window, cx);
                    });
                }
                if self.active {
                    div()
                        .id("active-conversation-overlay")
                        .absolute()
                        .bottom_0()
                        .w(animated_width.value + font_size * 2.)
                        .max_w_full()
                        .min_w_0()
                        .when(width_animating, |this| this.overflow_x_hidden())
                        .opacity(activation)
                        .occlude()
                        .on_mouse_down_out(cx.listener(Self::outside_click))
                        .child(item)
                        .into_any_element()
                } else {
                    let shortcut = window
                        .focused(cx)
                        .and_then(|focus| Kbd::binding_for_action_in(&ToggleFocus, &focus, window))
                        .or_else(|| {
                            Kbd::binding_for_action(&ToggleFocus, Some("Workspace"), window)
                        });
                    let theme = KitTheme::global(cx);
                    let shape = self.closing_input_shape.unwrap_or(OverlayInputShape {
                        height: font_size * 2.75,
                        corner_radius: px(22.),
                    });
                    let height = if returning_to_handle {
                        px(6.).lerp(&shape.height, closing_height)
                    } else {
                        px(6.).lerp(&(font_size * 2.), reveal)
                    };
                    let corner_radius = if returning_to_handle {
                        px(3.).lerp(&shape.corner_radius, closing_height)
                    } else {
                        height / 2.
                    };
                    let surface_visibility = if returning_to_handle {
                        closing_height
                    } else {
                        reveal
                    };
                    let background = theme
                        .muted_foreground
                        .opacity(0.4)
                        .lerp(&theme.group_box, surface_visibility);
                    let surface = div()
                        .flex()
                        .items_center()
                        .w_full()
                        .h(height)
                        .rounded(corner_radius)
                        .border_1()
                        .border_color(theme.input.opacity(surface_visibility))
                        .bg(background)
                        .overflow_hidden()
                        .when(!returning_to_handle, |this| {
                            this.child(
                                h_flex()
                                    .w_full()
                                    .min_w_0()
                                    .px_3()
                                    .gap_3()
                                    .justify_between()
                                    .opacity(reveal)
                                    .ui_text(UiTextSize::Body, UiTextTone::Secondary, cx)
                                    .child(
                                        div()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .child(status.unwrap_or_else(|| "Message zdsh".into())),
                                    )
                                    .children(shortcut.map(|shortcut| {
                                        div()
                                            .flex_none()
                                            .ui_text(UiTextSize::Caption, UiTextTone::Muted, cx)
                                            .child(shortcut.appearance(false))
                                    })),
                            )
                        });
                    div()
                        .id("conversation-overlay-launcher")
                        .absolute()
                        .bottom_0()
                        .w(animated_width.value)
                        .max_w_full()
                        .h(height.max(px(24.)))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(!returning_to_handle, |this| {
                            this.occlude().on_hover(cx.listener(|this, hovered, _, cx| {
                                if this.hovered != *hovered {
                                    this.hovered = *hovered;
                                    cx.notify();
                                }
                            }))
                        })
                        .child(if returning_to_handle {
                            // Keep only the shrinking surface: dismissed editors, menus and
                            // launcher buttons must not intercept the tab during the exit.
                            surface.into_any_element()
                        } else {
                            Button::new("activate-conversation-overlay")
                                .w_full()
                                .h_full()
                                .rounded(font_size)
                                .focus_visible(|style| style.bg(theme.ring.opacity(0.2)))
                                .accessibility_label("Message zdsh")
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.activate(window, cx)),
                                )
                                .child(surface)
                                .into_any_element()
                        })
                        .into_any_element()
                }
            }))
    }
}
