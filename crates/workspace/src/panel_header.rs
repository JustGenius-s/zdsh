use gpui_icons::Icon as KitIcon;
use std::rc::Rc;

use gpui::{
    AnyView, App, ClickEvent, CursorStyle, ElementId, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled,
    Transformation, Window, div, prelude::FluentBuilder as _, size,
};
use gpui_kit::{
    assets::IconName as KitIconName,
    base::Selectable as _,
    component::{
        Disableable as _, Icon as KitMenuIcon, Side,
        button::{Button, ButtonCustomVariant, ButtonVariants as _},
        menu::{PopupMenu, PopupMenuItem},
    },
};
use theme::ActiveTheme as _;
use ui::{Clickable, IconName, Toggleable, rems_from_px};

/// Cursor's menus are at least this wide, and their submenus a little narrower. GPUI Kit's own
/// minimum is 8rem, which leaves short menus much narrower.
const MENU_MIN_WIDTH: f32 = 236.;
const SUBMENU_MIN_WIDTH: f32 = 200.;

/// Styles a GPUI Kit menu like Cursor's: at least as wide, scaling with the UI font, and with
/// check marks at the end of their rows.
pub fn style_menu(menu: PopupMenu, window: &Window) -> PopupMenu {
    cursor_menu(menu, MENU_MIN_WIDTH, window)
}

/// Styles a GPUI Kit submenu like [`style_menu`], at the narrower width of Cursor's submenus.
pub fn style_submenu(menu: PopupMenu, window: &Window) -> PopupMenu {
    cursor_menu(menu, SUBMENU_MIN_WIDTH, window)
}

/// GPUI Kit draws every menu icon in a 12px box, smaller than Cursor draws its menu icons, and
/// leaves 4px between the box and the label.
const KIT_MENU_ICON_BOX: f32 = 12.;
const KIT_MENU_ICON_GAP: f32 = 4.;
/// The box that shows a glyph at Cursor's size, as in the sidebar.
const MENU_GLYPH_BOX: f32 = 15.2;
/// Cursor's space between a menu icon's box and its label.
const MENU_ICON_GAP: f32 = 10.;
/// Cursor draws menu icons in its secondary color: its text color mixed 66% with transparent.
const MENU_ICON_OPACITY: f32 = 0.66;

/// A menu item's icon, as large, as far from its label and as muted as Cursor's.
pub fn menu_icon(icon: impl Into<KitMenuIcon>) -> KitMenuIcon {
    menu_icon_in_box(icon, MENU_GLYPH_BOX).opacity(MENU_ICON_OPACITY)
}

/// A menu item's icon in its own color, drawn as large as it would be in a `box_size` box. Only
/// the drawing is scaled, and margins widen the kit's box to Cursor's slot, so every icon takes
/// the same room and the labels after them line up.
pub fn menu_icon_in_box(icon: impl Into<KitMenuIcon>, box_size: f32) -> KitMenuIcon {
    let scale = box_size / KIT_MENU_ICON_BOX;
    let inset = (MENU_GLYPH_BOX - KIT_MENU_ICON_BOX) / 2.;
    icon.into()
        .transform(Transformation::scale(size(scale, scale)))
        .ml(rems_from_px(inset))
        .mr(rems_from_px(inset + MENU_ICON_GAP - KIT_MENU_ICON_GAP))
}

/// An empty icon slot for a menu row without an icon, so its label lines up with those of the
/// rows that have one. The kit's own empty slot is narrower.
pub fn menu_icon_spacer() -> KitMenuIcon {
    menu_icon_in_box(KitMenuIcon::empty(), MENU_GLYPH_BOX)
}

/// A section title in a menu whose rows have icons, lined up with the rows' labels.
pub fn menu_heading(label: impl Into<SharedString>) -> PopupMenuItem {
    let label = label.into();
    PopupMenuItem::element(move |_, _| label.clone())
        .icon(menu_icon_spacer())
        .disabled(true)
}

fn cursor_menu(menu: PopupMenu, min_width: f32, window: &Window) -> PopupMenu {
    menu.min_w(rems_from_px(min_width).to_pixels(window.rem_size()))
        .check_side(Side::Right)
}

/// A transparent button whose hover tint matches the workspace's other icon controls.
pub fn ghost_button_style(cx: &App) -> ButtonCustomVariant {
    let colors = cx.theme().colors();
    // Kit halves its ghost hover tint in dark mode. A custom variant keeps icon controls
    // consistent without doubling the accent token shared by menus and lists.
    ButtonCustomVariant::new(cx)
        .foreground(colors.text)
        .hover(colors.ghost_element_hover)
        .active(colors.ghost_element_selected)
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
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let button = Button::new(self.id.clone())
            .custom(ghost_button_style(cx))
            .accessibility_label(self.label.clone())
            .dropdown_caret(true)
            .loading(self.loading)
            .selected(self.selected)
            .when_some(
                self.icon.or(self.loading.then_some(IconName::LoadCircle)),
                |button, icon| {
                    button.child(
                        KitIcon::empty()
                            .id("button-icon")
                            .path(
                                if self.loading {
                                    IconName::LoadCircle
                                } else {
                                    icon
                                }
                                .path(),
                            )
                            .spinning(self.loading)
                            .size_4(),
                    )
                },
            )
            .child(self.label)
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

/// A GPUI Kit icon button that can trigger a [`ui::PopoverMenu`], with a tooltip of Zed's for the
/// same reason as [`PickerTrigger`].
#[derive(IntoElement)]
pub struct KitIconButton {
    id: ElementId,
    icon: KitIconName,
    selected: bool,
    disabled: bool,
    on_click: Option<Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    tooltip: Option<Rc<dyn Fn(&mut Window, &mut App) -> AnyView>>,
}

impl KitIconButton {
    pub fn new(id: impl Into<ElementId>, icon: KitIconName) -> Self {
        Self {
            id: id.into(),
            icon,
            selected: false,
            disabled: false,
            on_click: None,
            tooltip: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.tooltip = Some(Rc::new(tooltip));
        self
    }
}

impl Clickable for KitIconButton {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }

    fn cursor_style(self, _cursor_style: CursorStyle) -> Self {
        self
    }
}

impl Toggleable for KitIconButton {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl RenderOnce for KitIconButton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let button = Button::new(self.id.clone())
            .custom(ghost_button_style(cx))
            .size_8()
            .p_0()
            .child(
                KitIcon::empty()
                    .id("button-icon")
                    .path(assets::kit_icon_path(self.icon))
                    .size_4(),
            )
            .selected(self.selected)
            .disabled(self.disabled)
            .when_some(self.on_click, |button, on_click| {
                button.on_click(move |event, window, cx| on_click(event, window, cx))
            });
        div()
            .id(self.id)
            .when_some(self.tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| tooltip(window, cx))
            })
            .child(button)
    }
}
