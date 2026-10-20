use std::rc::Rc;

use gpui::{
    AnyView, App, ClickEvent, CursorStyle, Div, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_kit::{
    base::Selectable as _,
    component::{
        Icon as KitIcon,
        button::{Button, ButtonVariants as _},
    },
};
use ui::{Clickable, IconName, Toggleable, h_flex};

/// The row atop a dock panel, naming what the panel shows.
pub fn panel_header() -> Div {
    h_flex().w_full().flex_none().h_10().px_2().gap_1()
}

/// A GPUI Kit button with a dropdown caret that opens one of Zed's pickers, as the trigger of a
/// [`ui::PopoverMenu`].
#[derive(IntoElement)]
pub struct PickerTrigger {
    id: ElementId,
    label: SharedString,
    icon: Option<IconName>,
    loading: bool,
    selected: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    tooltip: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyView>>,
}

impl PickerTrigger {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            icon: None,
            loading: false,
            selected: false,
            on_click: None,
            tooltip: None,
        }
    }

    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// GPUI Kit's own tooltips need its window root, which Zed's windows don't have, so the
    /// tooltip is Zed's.
    pub fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.tooltip = Some(Rc::new(tooltip));
        self
    }
}

impl Clickable for PickerTrigger {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    fn cursor_style(self, _cursor_style: CursorStyle) -> Self {
        self
    }
}

impl Toggleable for PickerTrigger {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for PickerTrigger {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let button = Button::new(self.id.clone())
            .ghost()
            .label(self.label)
            .dropdown_caret(true)
            .loading(self.loading)
            .selected(self.selected)
            .when_some(self.icon, |button, icon| {
                button.icon(KitIcon::empty().path(icon.path()))
            })
            .when_some(self.on_click, |button, on_click| {
                button.on_click(move |event, window, cx| on_click(event, window, cx))
            });
        div()
            .id(self.id)
            .min_w_0()
            .when_some(self.tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| tooltip(window, cx))
            })
            .child(button)
    }
}
