use std::sync::Arc;

use component::{Component, ComponentScope, example_group_with_title, single_example};
use gpui::{AnyElement, AnyView, ClickEvent, Hsla, MouseButton, MouseDownEvent, Pixels, Role, px};
use smallvec::SmallVec;

use crate::{Disclosure, prelude::*};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy, Default)]
pub enum ListItemSpacing {
    #[default]
    Dense,
    ExtraDense,
    Sparse,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum DockSide {
    Left,
    Right,
}

#[derive(Default)]
enum EndSlotVisibility {
    #[default]
    Always,
    OnHover,
    SwapOnHover(AnyElement),
}

#[derive(IntoElement, RegisterComponent)]
pub struct ListItem {
    id: ElementId,
    group_name: Option<SharedString>,
    disabled: bool,
    selected: bool,
    spacing: ListItemSpacing,
    indent_level: usize,
    indent_step_size: Pixels,
    /// A slot for content that appears before the children, like an icon or avatar.
    start_slot: Option<AnyElement>,
    /// A slot for content that appears after the children, usually on the other side of the header.
    /// This might be a button, a disclosure arrow, a face pile, etc.
    end_slot: Option<AnyElement>,
    end_slot_visibility: EndSlotVisibility,
    toggle: Option<bool>,
    inset: bool,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    on_hover: Option<Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>>,
    on_toggle: Option<Arc<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>>,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView + 'static>>,
    on_secondary_mouse_down: Option<Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>>,
    children: SmallVec<[AnyElement; 2]>,
    selectable: bool,
    always_show_disclosure_icon: bool,
    outlined: bool,
    overflow_x: bool,
    focused: Option<bool>,
    inactive_selection: bool,
    drop_target: bool,
    outline_color: Option<Hsla>,
    height: Option<DefiniteLength>,
    aria_role: Option<Role>,
    aria_label: Option<SharedString>,
    aria_keyshortcuts: Option<SharedString>,
    aria_checked: Option<bool>,
    aria_active_descendant: bool,
}

impl ListItem {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            group_name: None,
            disabled: false,
            selected: false,
            spacing: ListItemSpacing::Dense,
            indent_level: 0,
            indent_step_size: px(12.),
            start_slot: None,
            end_slot: None,
            end_slot_visibility: EndSlotVisibility::default(),
            toggle: None,
            inset: false,
            on_click: None,
            on_secondary_mouse_down: None,
            on_toggle: None,
            on_hover: None,
            tooltip: None,
            children: SmallVec::new(),
            selectable: true,
            always_show_disclosure_icon: false,
            outlined: false,
            overflow_x: false,
            focused: None,
            inactive_selection: false,
            drop_target: false,
            outline_color: None,
            height: None,
            aria_role: None,
            aria_label: None,
            aria_keyshortcuts: None,
            aria_checked: None,
            aria_active_descendant: false,
        }
    }

    /// Sets the accessible role reported to assistive technology (e.g.
    /// [`Role::MenuItem`] when this item is part of a menu). When unset, the
    /// item is not reported as a distinct node, matching list defaults.
    pub fn aria_role(mut self, role: Role) -> Self {
        self.aria_role = Some(role);
        self
    }

    /// Sets the label announced by assistive technology. List items render
    /// arbitrary children, so this should be set when a meaningful textual
    /// label exists.
    pub fn aria_label(mut self, label: impl Into<SharedString>) -> Self {
        self.aria_label = Some(label.into());
        self
    }

    /// Sets the keyboard shortcut announced by assistive technology for this
    /// item, e.g. a menu item's accelerator. Requires [`Self::aria_role`] to be
    /// set. Use a human-friendly display string (the accelerator shown to
    /// sighted users), e.g. `"Ctrl-S"`.
    pub fn aria_keyshortcuts(mut self, keyshortcuts: impl Into<SharedString>) -> Self {
        self.aria_keyshortcuts = Some(keyshortcuts.into());
        self
    }

    /// Sets the checked state reported to assistive technology, independent of
    /// the visual disclosure [`Self::toggle`]. Use this for checkable items
    /// such as a [`Role::MenuItemCheckBox`] entry so screen readers announce
    /// "checked"/"not checked" accurately.
    pub fn aria_checked(mut self, checked: bool) -> Self {
        self.aria_checked = Some(checked);
        self
    }

    /// Reports this item as the accessibility focus, overriding the element
    /// that holds real keyboard focus. Requires [`Self::aria_role`] to be set.
    ///
    /// Use this on the selected item of a composite widget (e.g. a menu) that
    /// keeps keyboard focus on its container. It is honored only while the
    /// container actually holds focus, so it is safe to set unconditionally on
    /// the selected item. See [`StatefulInteractiveElement::aria_active_descendant`].
    pub fn aria_active_descendant(mut self) -> Self {
        self.aria_active_descendant = true;
        self
    }

    pub fn group_name(mut self, group_name: impl Into<SharedString>) -> Self {
        self.group_name = Some(group_name.into());
        self
    }

    pub fn spacing(mut self, spacing: ListItemSpacing) -> Self {
        self.spacing = spacing;
        self
    }

    pub fn selectable(mut self, has_hover: bool) -> Self {
        self.selectable = has_hover;
        self
    }

    pub fn always_show_disclosure_icon(mut self, show: bool) -> Self {
        self.always_show_disclosure_icon = show;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    pub fn on_hover(mut self, handler: impl Fn(&bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_hover = Some(Box::new(handler));
        self
    }

    pub fn on_secondary_mouse_down(
        mut self,
        handler: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_secondary_mouse_down = Some(Box::new(handler));
        self
    }

    pub fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.tooltip = Some(Box::new(tooltip));
        self
    }

    pub fn inset(mut self, inset: bool) -> Self {
        self.inset = inset;
        self
    }

    pub fn indent_level(mut self, indent_level: usize) -> Self {
        self.indent_level = indent_level;
        self
    }

    pub fn indent_step_size(mut self, indent_step_size: Pixels) -> Self {
        self.indent_step_size = indent_step_size;
        self
    }

    pub fn toggle(mut self, toggle: impl Into<Option<bool>>) -> Self {
        self.toggle = toggle.into();
        self
    }

    pub fn on_toggle(
        mut self,
        on_toggle: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_toggle = Some(Arc::new(on_toggle));
        self
    }

    pub fn start_slot<E: IntoElement>(mut self, start_slot: impl Into<Option<E>>) -> Self {
        self.start_slot = start_slot.into().map(IntoElement::into_any_element);
        self
    }

    pub fn end_slot<E: IntoElement>(mut self, end_slot: impl Into<Option<E>>) -> Self {
        self.end_slot = end_slot.into().map(IntoElement::into_any_element);
        self
    }

    pub fn end_slot_on_hover<E: IntoElement>(mut self, end_slot_on_hover: E) -> Self {
        self.end_slot_visibility =
            EndSlotVisibility::SwapOnHover(end_slot_on_hover.into_any_element());
        self
    }

    pub fn show_end_slot_on_hover(mut self) -> Self {
        self.end_slot_visibility = EndSlotVisibility::OnHover;
        self
    }

    pub fn outlined(mut self) -> Self {
        self.outlined = true;
        self
    }

    pub fn overflow_x(mut self) -> Self {
        self.overflow_x = true;
        self
    }

    /// Marks the row the keyboard cursor is on; it is drawn like a selected row.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = Some(focused);
        self
    }

    /// Draws the selection lighter, for a list that does not currently have focus.
    pub fn inactive_selection(mut self, inactive: bool) -> Self {
        self.inactive_selection = inactive;
        self
    }

    /// Highlights the row as the target of an ongoing drag.
    pub fn drop_target(mut self, is_drop_target: bool) -> Self {
        self.drop_target = is_drop_target;
        self
    }

    /// Outlines the row, for states such as validation that need more than a wash.
    pub fn outline_color(mut self, color: impl Into<Option<Hsla>>) -> Self {
        self.outline_color = color.into();
        self
    }

    pub fn height(mut self, height: impl Into<DefiniteLength>) -> Self {
        self.height = Some(height.into());
        self
    }
}

impl Disableable for ListItem {
    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }
}

impl Toggleable for ListItem {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl ParentElement for ListItem {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements)
    }
}

/// An unfocused list's selection keeps this share of the selected wash (14% becomes 10%).
pub const INACTIVE_SELECTION_OPACITY: f32 = 0.72;

impl RenderOnce for ListItem {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let is_selected = self.selected || self.focused == Some(true);
        let selected_background = if self.inactive_selection {
            colors
                .ghost_element_selected
                .opacity(INACTIVE_SELECTION_OPACITY)
        } else {
            colors.ghost_element_selected
        };
        let indent = self.indent_level as f32 * self.indent_step_size;
        let content_padding = DynamicSpacing::Base06.rems(cx).to_pixels(window.rem_size());

        // Every row is an inset, rounded wash: the outer element keeps the inset from the
        // list's edges and the inner element carries the hover and selection state.
        h_flex()
            .id(self.id)
            .when_some(self.group_name, |this, group| this.group(group))
            .w_full()
            .when_some(self.height, |this, height| this.h(height))
            .relative()
            .px(DynamicSpacing::Base06.rems(cx))
            // An inset item draws its indent outside the wash; otherwise the wash spans the
            // indent so nested rows line up.
            .when(self.inset, |this| this.ml(indent))
            .when_some(self.on_hover, |this, on_hover| this.on_hover(on_hover))
            .child(
                h_flex()
                    .id("inner_list_item")
                    // The accessible role/label live here, alongside the click
                    // handler, so assistive technology reports one actionable
                    // node (e.g. a menu item) rather than an inert container.
                    .when_some(self.aria_role, |this, role| this.role(role))
                    .when(
                        self.aria_role.is_some() && self.aria_active_descendant,
                        |this| this.aria_active_descendant(),
                    )
                    .when_some(self.aria_label, |this, label| this.aria_label(label))
                    .when_some(self.aria_keyshortcuts, |this, keyshortcuts| {
                        this.aria_keyshortcuts(keyshortcuts)
                    })
                    .when(self.aria_role.is_some(), |this| {
                        this.aria_selected(self.selected).when_some(
                            self.aria_checked.or(self.toggle),
                            |this, toggled| {
                                this.aria_toggled(if toggled {
                                    gpui::Toggled::True
                                } else {
                                    gpui::Toggled::False
                                })
                            },
                        )
                    })
                    .group("list_item")
                    .w_full()
                    .relative()
                    .gap_1()
                    .rounded_md()
                    .map(|this| {
                        if self.inset {
                            this.px(content_padding)
                        } else {
                            this.pl(indent + content_padding).pr(content_padding)
                        }
                    })
                    .map(|this| match self.spacing {
                        ListItemSpacing::Dense => this,
                        ListItemSpacing::ExtraDense => this.py_neg_px(),
                        ListItemSpacing::Sparse => this.py_1(),
                    })
                    .when(self.selectable && !self.disabled, |this| {
                        this.hover(|style| style.bg(colors.ghost_element_hover))
                            .active(|style| style.bg(colors.ghost_element_active))
                            .when(is_selected, |this| this.bg(selected_background))
                    })
                    .when(self.drop_target, |this| {
                        this.bg(colors.drop_target_background)
                    })
                    .when_some(self.outline_color, |this, color| {
                        this.border_1().border_color(color)
                    })
                    .when_some(
                        self.on_click.filter(|_| !self.disabled),
                        |this, on_click| this.cursor_pointer().on_click(on_click),
                    )
                    .when(self.outlined, |this| {
                        this.border_1()
                            .border_color(colors.border)
                            .overflow_hidden()
                    })
                    .when_some(self.on_secondary_mouse_down, |this, on_mouse_down| {
                        this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                            (on_mouse_down)(event, window, cx)
                        })
                    })
                    .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
                    .children(self.toggle.map(|is_open| {
                        div()
                            .flex()
                            .absolute()
                            .map(|this| {
                                let disclosure_offset = rems(1.).to_pixels(window.rem_size());
                                if self.inset {
                                    this.left(-disclosure_offset)
                                } else {
                                    this.left(indent - disclosure_offset)
                                }
                            })
                            .when(is_open && !self.always_show_disclosure_icon, |this| {
                                this.visible_on_hover("")
                            })
                            .child(
                                Disclosure::new("toggle", is_open)
                                    .on_toggle_expanded(self.on_toggle),
                            )
                    }))
                    .child(
                        h_flex()
                            .flex_grow_1()
                            .flex_shrink_0()
                            .flex_basis(relative(0.25))
                            .gap(DynamicSpacing::Base06.rems(cx))
                            .map(|list_content| {
                                if self.overflow_x {
                                    list_content
                                } else {
                                    list_content.overflow_hidden()
                                }
                            })
                            .children(self.start_slot)
                            .children(self.children),
                    )
                    .when(self.end_slot.is_some(), |this| this.justify_between())
                    .map(|this| match self.end_slot_visibility {
                        EndSlotVisibility::Always if let Some(end_slot) = self.end_slot => {
                            this.child(h_flex().flex_shrink_1().overflow_hidden().child(end_slot))
                        }
                        EndSlotVisibility::OnHover if let Some(end_slot) = self.end_slot => this
                            .child(
                                h_flex()
                                    .flex_shrink_1()
                                    .overflow_hidden()
                                    .visible_on_hover("list_item")
                                    .child(end_slot),
                            ),
                        EndSlotVisibility::SwapOnHover(hover_slot) => this.child(
                            h_flex()
                                .relative()
                                .flex_shrink_1()
                                .child(h_flex().visible_on_hover("list_item").child(hover_slot))
                                .when_some(self.end_slot, |this, end_slot| {
                                    this.child(
                                        h_flex()
                                            .absolute()
                                            .inset_0()
                                            .justify_end()
                                            .overflow_hidden()
                                            .group_hover("list_item", |this| this.invisible())
                                            .child(end_slot),
                                    )
                                }),
                        ),
                        _ => this,
                    }),
            )
    }
}

impl Component for ListItem {
    fn scope() -> ComponentScope {
        ComponentScope::DataDisplay
    }

    fn description() -> &'static str {
        "A flexible list item component with support for icons, actions, \
        disclosure toggles, and hierarchical display."
    }

    fn preview(_window: &mut Window, _cx: &mut App) -> AnyElement {
        v_flex()
            .gap_6()
            .children(vec![
                example_group_with_title(
                    "Basic List Items",
                    vec![
                        single_example(
                            "Simple",
                            ListItem::new("simple")
                                .child(Label::new("Simple list item"))
                                .into_any_element(),
                        ),
                        single_example(
                            "With Icon",
                            ListItem::new("with_icon")
                                .start_slot(Icon::new(IconName::File))
                                .child(Label::new("List item with icon"))
                                .into_any_element(),
                        ),
                        single_example(
                            "Selected",
                            ListItem::new("selected")
                                .toggle_state(true)
                                .start_slot(Icon::new(IconName::Check))
                                .child(Label::new("Selected item"))
                                .into_any_element(),
                        ),
                    ],
                ),
                example_group_with_title(
                    "List Item Spacing",
                    vec![
                        single_example(
                            "Dense",
                            ListItem::new("dense")
                                .spacing(ListItemSpacing::Dense)
                                .child(Label::new("Dense spacing"))
                                .into_any_element(),
                        ),
                        single_example(
                            "Extra Dense",
                            ListItem::new("extra_dense")
                                .spacing(ListItemSpacing::ExtraDense)
                                .child(Label::new("Extra dense spacing"))
                                .into_any_element(),
                        ),
                        single_example(
                            "Sparse",
                            ListItem::new("sparse")
                                .spacing(ListItemSpacing::Sparse)
                                .child(Label::new("Sparse spacing"))
                                .into_any_element(),
                        ),
                    ],
                ),
                example_group_with_title(
                    "With Slots",
                    vec![
                        single_example(
                            "End Slot",
                            ListItem::new("end_slot")
                                .child(Label::new("Item with end slot"))
                                .end_slot(Icon::new(IconName::ChevronRight))
                                .into_any_element(),
                        ),
                        single_example(
                            "With Toggle",
                            ListItem::new("with_toggle")
                                .toggle(Some(true))
                                .child(Label::new("Expandable item"))
                                .into_any_element(),
                        ),
                    ],
                ),
                example_group_with_title(
                    "States",
                    vec![
                        single_example(
                            "Disabled",
                            ListItem::new("disabled")
                                .disabled(true)
                                .child(Label::new("Disabled item"))
                                .into_any_element(),
                        ),
                        single_example(
                            "Non-selectable",
                            ListItem::new("non_selectable")
                                .selectable(false)
                                .child(Label::new("Non-selectable item"))
                                .into_any_element(),
                        ),
                    ],
                ),
            ])
            .into_any_element()
    }
}
