use super::*;
use crate::agent_panel::{AgentChoice, AgentChoiceIcon};
use crate::profile_selector::{is_selectable_profile, profile_description, profile_icon};
use agent_settings::{AgentProfile, builtin_profiles};
use context_server::ContextServerId;
use editor::{actions::ConfirmCompletion, code_context_menus::CompletionsMenuRow};
use fuzzy::{StringMatchCandidate, match_strings};
use gpui::{AnyView, ListSizingBehavior, anchored, canvas, deferred, relative, uniform_list};
use gpui_kit::{
    base::Selectable as _,
    component::{
        Disableable as _, Icon as KitIcon, IndexPath, Size as KitSize, Theme as KitTheme,
        button::{Button as KitButton, ButtonRounded, ButtonVariant, DropdownButton},
        list::{List as KitList, ListDelegate, ListEvent, ListItem as KitListItem, ListState},
        menu::{DropdownMenu as _, PopupMenu, PopupMenuItem},
        switch::Switch as KitSwitch,
        tag::Tag,
    },
};
use project::context_server_store::{ContextServerStatus, ContextServerStore};
use settings::ContextServerSettingsContent;
use std::sync::atomic::AtomicBool;
use ui::{HighlightedLabel, ProgressBar};

const FOLLOW_UP_PLACEHOLDER: &str = "Send follow-up";
const CURSOR_USAGE_DASHBOARD_URL: &str = "https://cursor.com/dashboard?tab=usage";

impl ThreadView {
    /// Returns the composer, plus the changes chips when they should float over the transcript's
    /// bottom edge instead of sitting in the composer. `cards_above_composer` says whether the
    /// thread stacks anything between the transcript and the composer.
    pub(crate) fn render_message_editor(
        &mut self,
        cards_above_composer: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (AnyElement, Option<AnyElement>) {
        if self.is_subagent() {
            return (div().into_any_element(), None);
        }

        let editor_expanded = self.editor_expanded;
        let max_content_width = AgentSettings::get_global(cx).max_content_width;

        let context_usage_card = self
            .context_usage_expanded
            .then(|| self.render_context_usage_card(cx))
            .flatten();
        let cursor_usage_card = self
            .cursor_usage_expanded
            .then(|| self.render_cursor_usage_card(cx))
            .flatten();
        let add_menu_card = self.add_menu.clone().map(|list| {
            composer_card(cx)
                // A press in the menu would otherwise move focus off its search field, which
                // closes the menu before the click lands.
                .capture_any_mouse_down(|_, window, _| window.prevent_default())
                // The list passes Escape on, and Zed's `menu::Cancel` would then cancel the
                // running turn.
                .on_action(cx.listener(|this, _: &menu::Cancel, window, cx| {
                    this.close_add_menu(window, cx);
                }))
                .child(
                    KitList::new(&list)
                        .search_placeholder("Search modes, context, skills…")
                        .max_h(rems(24.))
                        .px_1()
                        .pb_1(),
                )
        });
        let completions_menu_card = self.render_completions_menu(window, cx);
        let composer_bounds = self.composer_bounds.clone();
        let is_empty_thread = !self.has_visible_conversation(cx);
        let add_menu_docked = self.add_menu.is_some() && !is_empty_thread;
        let (floating_menus, docked_menus) = if is_empty_thread {
            let menu_width = composer_bounds.get().map(|bounds| bounds.size.width);
            let floating_add_menu = add_menu_card.map(|card| {
                let composer_bounds = composer_bounds.clone();
                card.on_mouse_down_out(cx.listener(move |this, _, window, cx| {
                    let pressed_composer = composer_bounds
                        .get()
                        .is_some_and(|bounds| bounds.contains(&window.mouse_position()));
                    if !pressed_composer {
                        this.close_add_menu(window, cx);
                    }
                }))
            });
            let floating_menus = floating_add_menu
                .into_iter()
                .chain(completions_menu_card)
                .map(|card| {
                    float_below_composer(
                        card.occlude()
                            .when_some(menu_width, |card, width| card.w(width)),
                    )
                })
                .collect::<Vec<_>>();
            (floating_menus, Vec::new())
        } else {
            let docked_menus = add_menu_card
                .into_iter()
                .chain(completions_menu_card)
                .map(|card| card.w_full())
                .collect::<Vec<_>>();
            (Vec::new(), docked_menus)
        };
        let agent_selector = is_empty_thread
            .then(|| self.render_agent_selector(cx))
            .flatten();
        // The chips always sit right above the input. They float over the transcript only when
        // nothing is stacked between the two; otherwise they'd end up above those cards.
        let changes_chips = self.render_changes_chips(cx);
        let float_changes_chips = !is_empty_thread
            && !cards_above_composer
            && context_usage_card.is_none()
            && cursor_usage_card.is_none()
            && docked_menus.is_empty();
        let (changes_chips, floating_changes_chips) = if float_changes_chips {
            (None, changes_chips)
        } else {
            (changes_chips, None)
        };
        let composer = if is_empty_thread {
            div()
                .relative()
                .w_full()
                .min_w_0()
                .child(self.render_composer_card(cx))
                .children(floating_menus)
                .into_any_element()
        } else if self.update_composer_stacked(cx) {
            self.render_composer_card(cx)
        } else {
            self.render_composer_pill(cx)
        };
        let sandbox_status = self.render_sandbox_status(cx);
        let footer = self.render_composer_footer(sandbox_status, cx);

        let composer = v_flex()
            .flex_none()
            .w_full()
            .px_4()
            .pt_2()
            .pb_2()
            .items_center()
            .on_action(cx.listener(Self::handle_message_editor_move_up))
            .on_action(cx.listener(Self::expand_message_editor))
            .when(editor_expanded, |this| this.h(vh(0.8, window)))
            .child(
                v_flex()
                    .relative()
                    .w_full()
                    .when_some(max_content_width, |this, max_width| this.max_w(max_width))
                    .min_w_0()
                    .when(editor_expanded, |this| this.flex_1().min_h_0())
                    .gap_2()
                    .child(
                        canvas(
                            move |bounds, _window, _cx| composer_bounds.set(Some(bounds)),
                            |_bounds, _state, _window, _cx| {},
                        )
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full(),
                    )
                    .when(add_menu_docked, |this| {
                        this.on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_add_menu(window, cx);
                        }))
                    })
                    .children(context_usage_card)
                    .children(cursor_usage_card)
                    .children(docked_menus)
                    .children(agent_selector)
                    .children(changes_chips)
                    .child(composer)
                    .child(footer),
            )
            .into_any();
        (composer, floating_changes_chips)
    }

    /// Stacking widens the input, which can undo the soft wrap that triggered it, so the composer
    /// stays stacked until the input is cleared rather than flipping back and forth.
    fn update_composer_stacked(&mut self, cx: &mut Context<Self>) -> bool {
        if self.message_editor.read(cx).is_empty(cx) {
            self.composer_stacked = false;
        }
        if !self.composer_stacked {
            self.composer_stacked = self.editor_expanded
                || self
                    .message_editor
                    .update(cx, |message_editor, cx| message_editor.spans_lines(cx));
        }
        self.composer_stacked
    }

    fn render_composer_pill(&self, cx: &mut Context<Self>) -> AnyElement {
        let editor_expanded = self.editor_expanded;
        let (background, border) = composer_surface(cx);
        h_flex()
            .w_full()
            .min_w_0()
            .when(editor_expanded, |this| this.flex_1().min_h_0())
            .items_end()
            .gap_1p5()
            .p(px(6.))
            .rounded(px(22.))
            .border_1()
            .border_color(border)
            .bg(background)
            .child(self.render_add_menu_button(cx))
            .children(self.render_mode_chip(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .when(editor_expanded, |this| this.h_full())
                    .px_1()
                    .py(px(4.))
                    .child(self.message_editor.clone()),
            )
            .child(self.render_trailing_controls(cx))
            .into_any_element()
    }

    /// A card like Cursor's, with the message editor above its controls: the composer of an
    /// empty thread, and the docked composer once its input takes more than one line.
    fn render_composer_card(&self, cx: &mut Context<Self>) -> AnyElement {
        let editor_expanded = self.editor_expanded;
        let (background, border) = composer_surface(cx);
        v_flex()
            .w_full()
            .min_w_0()
            .when(editor_expanded, |this| this.flex_1().min_h_0())
            .gap_2()
            .p_2()
            .rounded(px(16.))
            .border_1()
            .border_color(border)
            .bg(background)
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .min_h(rems(3.))
                    .when(editor_expanded, |this| this.flex_1().min_h_0())
                    .px_1p5()
                    .pt_1()
                    .child(self.message_editor.clone()),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .justify_between()
                    .gap_1()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1()
                            .child(self.render_add_menu_button(cx))
                            .children(self.render_mode_chip(cx)),
                    )
                    .child(self.render_trailing_controls(cx)),
            )
            .into_any_element()
    }

    /// The agent the empty thread runs, switched from a menu of the installed agents. Switching
    /// starts the thread over with the new agent, in the same tab.
    fn render_agent_selector(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let conversation = self.server_view.upgrade()?;
        let panel = self.workspace.upgrade()?.read(cx).panel::<AgentPanel>(cx)?;
        let current_agent = conversation.read(cx).agent_key().clone();
        let choices = Rc::new(panel.read(cx).agent_choices(cx));
        let current_choice = choices.iter().find(|choice| choice.agent == current_agent);
        let label = current_choice
            .map(|choice| choice.label.clone())
            .unwrap_or_else(|| current_agent.label());
        let icon = current_choice
            .map(|choice| choice.icon.clone())
            .or_else(|| current_agent.icon().map(AgentChoiceIcon::Builtin));

        let panel = panel.downgrade();
        let conversation = conversation.downgrade();
        Some(
            h_flex()
                .w_full()
                .child(
                    KitButton::new("agent-selector")
                        .ghost()
                        .small()
                        .dropdown_caret(true)
                        .child(
                            h_flex()
                                .gap_1p5()
                                .children(icon.map(|icon| agent_choice_icon(&icon)))
                                .child(Label::new(label)),
                        )
                        .dropdown_menu_with_anchor(gpui::Anchor::TopLeft, move |menu, _, _| {
                            agent_menu(
                                menu,
                                &choices,
                                &current_agent,
                                panel.clone(),
                                conversation.clone(),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    fn render_trailing_controls(&self, cx: &mut Context<Self>) -> Div {
        let focus_handle = self.message_editor.focus_handle(cx);
        let editor_expanded = self.editor_expanded;
        let (expand_icon, expand_tooltip) = if editor_expanded {
            (IconName::Minimize, "Minimize Message Editor")
        } else {
            (IconName::Maximize, "Expand Message Editor")
        };

        let editor = self.message_editor.read(cx).editor().read(cx);
        let composer_is_read_only = editor.read_only(cx);
        let is_multiline = editor.buffer().read(cx).snapshot(cx).max_point().row > 0;
        let show_expand_toggle =
            self.has_visible_conversation(cx) && (editor_expanded || is_multiline);

        let discard_draft_button = composer_is_read_only.then(|| {
            div()
                .debug_selector(|| "discard-protected-draft".into())
                .child(
                    PillButton::new("discard-protected-draft")
                        .size(KitSize::Small)
                        .label("Discard draft")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this._draft_resolve_task.take();
                            this.message_editor.update(cx, |editor, cx| {
                                editor.set_read_only(false, cx);
                                editor.set_message(Vec::new(), window, cx);
                            });
                            this.clear_thread_error(cx);
                        })),
                )
        });

        let expand_toggle = show_expand_toggle.then(|| {
            PillButton::new("toggle-height")
                .size(KitSize::Small)
                .icon(expand_icon)
                .tooltip(move |_window, cx| {
                    Tooltip::for_action_in(expand_tooltip, &ExpandMessageEditor, &focus_handle, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.expand_message_editor(&ExpandMessageEditor, window, cx);
                }))
        });

        h_flex()
            .flex_none()
            .gap_1()
            .children(discard_draft_button)
            .children(expand_toggle)
            .children(self.render_fast_mode_control(cx))
            .map(|this| match self.config_options_view.clone() {
                Some(config_view) => this
                    .child(config_view)
                    .children(self.render_thinking_control(cx)),
                None => this
                    .children(self.mode_selector.clone())
                    .child(self.render_model_and_effort_control(cx)),
            })
            .child(self.render_send_button(cx))
    }

    /// Swaps the empty-thread placeholder for a follow-up prompt once the
    /// conversation has content, mirroring how the composer reads mid-thread.
    pub(crate) fn sync_placeholder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shows_follow_up = self.has_visible_conversation(cx);
        if self.placeholder_shows_follow_up == Some(shows_follow_up) {
            return;
        }
        self.placeholder_shows_follow_up = Some(shows_follow_up);

        let placeholder = if shows_follow_up {
            FOLLOW_UP_PLACEHOLDER
        } else {
            self.empty_thread_placeholder.as_str()
        };
        self.message_editor.update(cx, |editor, cx| {
            editor.set_placeholder_text(placeholder, window, cx);
        });
    }

    pub(crate) fn set_empty_thread_placeholder(
        &mut self,
        placeholder: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.empty_thread_placeholder = placeholder;
        self.placeholder_shows_follow_up = None;
        self.sync_placeholder(window, cx);
    }

    pub(super) fn render_send_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let message_editor = self.message_editor.read(cx);
        let is_editor_empty = message_editor.is_empty(cx);
        let focus_handle = message_editor.focus_handle(cx);

        let is_generating = self.thread.read(cx).status() != ThreadStatus::Idle;

        if self.is_loading_contents {
            return div()
                .id("loading-message-content")
                .px_1()
                .tooltip(Tooltip::text("Loading Added Context…"))
                .child(loading_contents_spinner(IconSize::default()))
                .into_any_element();
        }

        if is_generating && is_editor_empty {
            return PillButton::new("stop-generation")
                .variant(ButtonVariant::Primary)
                .icon(IconName::Stop)
                .tooltip(move |_window, cx| {
                    Tooltip::for_action("Stop Generation", &editor::actions::Cancel, cx)
                })
                .on_click(cx.listener(|this, _event, _, cx| this.cancel_generation(cx)))
                .into_any_element();
        }

        let can_send = !is_editor_empty || is_generating;

        PillButton::new("send-message")
            .variant(ButtonVariant::Primary)
            .icon(IconName::ArrowUp)
            .disabled(!can_send)
            .tooltip(move |window, cx| {
                if !can_send {
                    Tooltip::for_action("Type to Send", &Chat, cx)
                } else if is_generating {
                    let focus_handle = focus_handle.clone();
                    Tooltip::element(move |_window, cx| {
                        v_flex()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .justify_between()
                                    .child(Label::new("Queue and Send"))
                                    .child(KeyBinding::for_action_in(&Chat, &focus_handle, cx)),
                            )
                            .child(
                                h_flex()
                                    .pt_1()
                                    .gap_2()
                                    .justify_between()
                                    .border_t_1()
                                    .border_color(cx.theme().colors().border_variant)
                                    .child(Label::new("Send Immediately"))
                                    .child(KeyBinding::for_action_in(
                                        &SendImmediately,
                                        &focus_handle,
                                        cx,
                                    )),
                            )
                            .into_any_element()
                    })(window, cx)
                } else {
                    Tooltip::for_action("Send Message", &Chat, cx)
                }
            })
            .on_click(cx.listener(|this, _, window, cx| {
                this.send(window, cx);
            }))
            .into_any_element()
    }

    /// The message editor's `@` and `/` menu, which the editor leaves to the composer to draw.
    /// The editor still filters it and handles the keyboard.
    fn render_completions_menu(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<Div> {
        let editor = self.message_editor.read(cx).editor().clone();
        let state = editor.read(cx).completions_menu_state()?;
        let selected_index = state.selected_index;
        let row_count = state.rows.len();
        let rows = Rc::new(state.rows);
        let list = uniform_list("composer-completions", row_count, move |range, _, cx| {
            range
                .filter_map(|index| {
                    let row = rows.get(index)?;
                    Some(completions_menu_row(
                        row,
                        index,
                        index == selected_index,
                        editor.clone(),
                        cx,
                    ))
                })
                .collect()
        })
        .track_scroll(&state.scroll_handle)
        .with_sizing_behavior(ListSizingBehavior::Infer)
        .max_h(rems(24.));

        Some(
            composer_card(cx)
                // The editor closes the menu when it loses focus, which a press here would
                // otherwise cause before the click lands.
                .capture_any_mouse_down(|_, window, _| window.prevent_default())
                .p_1()
                .child(
                    div().child(list).custom_scrollbars(
                        Scrollbars::new(ScrollAxes::Vertical)
                            .tracked_scroll_handle(&state.scroll_handle),
                        window,
                        cx,
                    ),
                ),
        )
    }

    fn render_add_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.message_editor.focus_handle(cx);

        div()
            .debug_selector(|| "composer-add-context".into())
            .child(
                PillButton::new("add-context")
                    .variant(ButtonVariant::Secondary)
                    .icon(IconName::Plus)
                    .toggle_state(self.add_menu.is_some())
                    .tooltip(move |_window, cx| {
                        Tooltip::for_action_in(
                            "Add Context",
                            &OpenAddContextMenu,
                            &focus_handle,
                            cx,
                        )
                    })
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.toggle_add_menu(window, cx);
                    })),
            )
    }

    /// Mirrors Cursor: the default mode stays implicit, any other mode is shown as a tag next
    /// to the add button so it is obvious the agent behaves differently. Removing the tag
    /// returns to the default mode.
    fn render_mode_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let profile_selector = self.profile_selector.as_ref()?.read(cx);
        if !profile_selector.profiles_available(cx) {
            return None;
        }
        let profile_id = profile_selector.active_profile_id(cx);
        if profile_id.as_str() == builtin_profiles::WRITE {
            return None;
        }
        let name = AgentSettings::get_global(cx)
            .profiles
            .get(&profile_id)?
            .name
            .clone();
        let (icon, color) = profile_icon(&profile_id);
        let tint = color.color(cx);

        Some(
            div()
                .id("composer-mode")
                .flex_none()
                .cursor_pointer()
                .tooltip(Tooltip::text("Change Mode"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_add_menu(window, cx);
                }))
                .child(
                    Tag::custom(tint.opacity(0.15), tint, gpui::transparent_black())
                        .rounded_full()
                        .h(px(32.))
                        .pl_2p5()
                        .pr_1()
                        .gap_1p5()
                        .child(Icon::new(icon).size(IconSize::Small).color(color))
                        .child(Label::new(name).color(color))
                        .child(
                            PillButton::new("composer-mode-remove")
                                .size(KitSize::XSmall)
                                .child(
                                    Icon::new(IconName::Close)
                                        .size(IconSize::XSmall)
                                        .color(color),
                                )
                                .tooltip(Tooltip::text("Remove Mode"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(profile_selector) = this.profile_selector.clone() {
                                        profile_selector.update(cx, |selector, cx| {
                                            selector.select_profile(
                                                AgentProfileId(builtin_profiles::WRITE.into()),
                                                cx,
                                            );
                                            cx.notify();
                                        });
                                    }
                                    cx.notify();
                                })),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn toggle_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_menu.is_some() {
            self.close_add_menu(window, cx);
            return;
        }

        let list = self.build_add_menu(window, cx);
        let search_focus = list.focus_handle(cx);
        self._add_menu_subscriptions = vec![
            cx.subscribe_in(&list, window, |this, _, event: &ListEvent, window, cx| {
                if matches!(event, ListEvent::Cancel) {
                    this.close_add_menu(window, cx);
                }
            }),
            cx.on_blur(&search_focus, window, |this, window, cx| {
                this.close_add_menu(window, cx);
            }),
        ];
        search_focus.focus(window, cx);
        self.add_menu = Some(list);
        self.context_usage_expanded = false;
        self.cursor_usage_expanded = false;
        cx.notify();
    }

    fn close_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(list) = self.add_menu.take() else {
            return;
        };
        self._add_menu_subscriptions.clear();
        // Only reclaim focus when the menu had it; a click elsewhere already
        // moved focus where the user wants it.
        if list.focus_handle(cx).is_focused(window) {
            self.message_editor.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn build_add_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ListState<AddMenuDelegate>> {
        let skills = self.session_capabilities.read().completion_skills();
        // Write is the default mode, which removing the mode tag returns to, so it isn't listed.
        let modes = self
            .profile_selector
            .as_ref()
            .filter(|selector| selector.read(cx).profiles_available(cx))
            .map(|_| {
                AgentProfile::available_profiles(cx)
                    .into_iter()
                    .filter(|(profile_id, _)| {
                        is_selectable_profile(profile_id)
                            && profile_id.as_str() != builtin_profiles::WRITE
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let model_name = self
            .model_selector
            .as_ref()
            .and_then(|selector| selector.read(cx).active_model(cx))
            .map(|model| model.name.clone());
        let context_server_store = self
            .project
            .upgrade()
            .map(|project| project.read(cx).context_server_store());

        let mut delegate = AddMenuDelegate {
            thread_view: cx.weak_entity(),
            message_editor: self.message_editor.clone(),
            profile_selector: self.profile_selector.clone(),
            context_server_store: context_server_store.clone(),
            modes,
            skills,
            model_name,
            view: AddMenuView::Root,
            sections: Vec::new(),
            selected_index: None,
            _store_subscription: None,
        };
        delegate.show_browse_entries(cx);

        cx.new(|cx| {
            let mut list = ListState::new(delegate, window, cx).searchable(true);
            if let Some(store) = context_server_store {
                list.delegate_mut()._store_subscription =
                    Some(cx.observe(&store, |_, _, cx| cx.notify()));
            }
            let first_selectable = list.delegate().first_selectable_index(cx);
            list.set_selected_index(first_selectable, window, cx);
            list
        })
    }

    fn render_changes_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let thread = self.thread.read(cx);
        if thread.parent_session_id().is_some() {
            return None;
        }

        let changed_buffers = thread
            .action_log()
            .read(cx)
            .changed_buffers(cx)
            .collect::<Vec<_>>();
        // A fresh thread has nothing of its own to commit yet.
        let has_uncommitted_changes = self.has_visible_conversation(cx)
            && self
                .project
                .upgrade()
                .and_then(|project| project.read(cx).active_repository(cx))
                .is_some_and(|repository| repository.read(cx).status_summary().count > 0);

        if changed_buffers.is_empty() && !has_uncommitted_changes {
            return None;
        }

        let changes_chip = (!changed_buffers.is_empty()).then(|| {
            let stats = DiffStats::all_files(changed_buffers.iter().cloned(), cx);
            let file_count = changed_buffers.len();

            PillButton::new("composer-changes")
                .variant(ButtonVariant::Secondary)
                .outline()
                .backdrop(cx.theme().colors().surface_background)
                .size(KitSize::Medium)
                .toggle_state(self.edits_expanded)
                .child(Label::new("Changes"))
                .child(Label::new(format!("+{}", stats.lines_added)).color(Color::Success))
                .child(Label::new(format!("-{}", stats.lines_removed)).color(Color::Error))
                .tooltip(move |_window, cx| {
                    Tooltip::simple(
                        format!(
                            "{} {} edited by the agent",
                            file_count,
                            pluralize("file", file_count)
                        ),
                        cx,
                    )
                })
                .on_click(cx.listener(|this, _, _, cx| {
                    this.edits_expanded = !this.edits_expanded;
                    cx.notify();
                }))
        });

        let commit_chip = has_uncommitted_changes.then(|| {
            DropdownButton::new("composer-commit")
                .secondary()
                .outline()
                .with_size(KitSize::Medium)
                // The halves are translucent outline buttons, so paint the thread's surface
                // behind the pair, rounded like them.
                .bg(cx.theme().colors().surface_background)
                .rounded(KitTheme::global(cx).radius)
                .button(
                    KitButton::new("composer-commit-action")
                        .label("Commit")
                        .on_click(|_, window, cx| {
                            window.dispatch_action(git::Commit.boxed_clone(), cx)
                        }),
                )
                .menu_accessibility_label("More Commit Options")
                .dropdown_menu_with_anchor(gpui::Anchor::BottomLeft, |menu, _, _| {
                    menu.menu("Amend Last Commit…", git::Amend.boxed_clone())
                        .separator()
                        .menu("Push", git::Push.boxed_clone())
                })
        });

        Some(
            h_flex()
                .gap_1p5()
                .children(changes_chip)
                .children(commit_chip)
                .into_any_element(),
        )
    }

    fn render_composer_footer(
        &self,
        sandbox_status: Option<AnyElement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        h_flex()
            .w_full()
            .min_w_0()
            .pl_1()
            .pr_1p5()
            .gap_2()
            .justify_between()
            .child(div().min_w_0().children(self.render_branch_picker(cx)))
            .child(
                h_flex()
                    .flex_none()
                    .gap_1()
                    .children(sandbox_status)
                    .children(self.render_follow_indicator(cx))
                    .children(self.render_cursor_usage_ring(cx))
                    .children(self.render_context_usage_ring(cx)),
            )
            .into_any_element()
    }

    fn render_branch_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let repository = self.project.upgrade()?.read(cx).active_repository(cx)?;
        let branch_name = {
            let repository = repository.read(cx);
            repository
                .branch
                .as_ref()
                .map(|branch| SharedString::from(branch.name().to_string()))
                .or_else(|| {
                    repository.head_commit.as_ref().map(|commit| {
                        SharedString::from(commit.sha.chars().take(7).collect::<String>())
                    })
                })?
        };
        let workspace = self.workspace.clone();
        let tooltip_label = branch_name.clone();

        Some(
            PopoverMenu::new("composer-branch-menu")
                .menu(move |window, cx| {
                    git_ui_core::build_branch_picker(
                        workspace.clone(),
                        Some(repository.clone()),
                        window,
                        cx,
                    )
                })
                .trigger(
                    workspace::PickerTrigger::new("composer-branch", branch_name)
                        .icon(IconName::GitBranch)
                        .tooltip(move |_window, cx| {
                            Tooltip::with_meta(
                                "Branch & Stash",
                                Some(&zed_actions::git::Branch),
                                format!("Currently Checked Out: {}", tooltip_label),
                                cx,
                            )
                        }),
                )
                .anchor(gpui::Anchor::BottomLeft)
                .into_any_element(),
        )
    }

    fn context_usage(&self, cx: &App) -> Option<ContextUsage> {
        let thread = self.thread.read(cx);
        let usage = thread.token_usage()?;
        let ratio = if usage.max_tokens > 0 {
            usage.used_tokens as f32 / usage.max_tokens as f32
        } else {
            0.0
        };

        Some(ContextUsage {
            ratio,
            used_tokens: usage.used_tokens,
            max_tokens: usage.max_tokens,
            percentage: format!("{}%", (ratio * 100.0).round() as u32),
            used: crate::humanize_token_count(usage.used_tokens),
            max: crate::humanize_token_count(usage.max_tokens),
        })
    }

    fn context_usage_color(ratio: f32, cx: &App) -> Hsla {
        if ratio >= 0.85 {
            cx.theme().status().warning
        } else {
            cx.theme().colors().text_muted
        }
    }

    fn render_context_usage_ring(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let usage = self.context_usage(cx)?;
        let progress_color = Self::context_usage_color(usage.ratio, cx);
        let tooltip_label = format!("Context Usage: {}", usage.percentage);

        Some(
            PillButton::new("context-usage-ring")
                .size(KitSize::Small)
                .toggle_state(self.context_usage_expanded)
                .child(
                    CircularProgress::new(usage.ratio.min(1.0), 1.0, px(16.), cx)
                        .stroke_width(px(2.))
                        .progress_color(progress_color),
                )
                .tooltip(Tooltip::text(tooltip_label))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.add_menu.is_some() {
                        this.close_add_menu(window, cx);
                    }
                    this.context_usage_expanded = !this.context_usage_expanded;
                    if this.context_usage_expanded {
                        this.cursor_usage_expanded = false;
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn render_context_usage_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let usage = self.context_usage(cx)?;
        let colors = cx.theme().colors();
        let bar_color = Self::context_usage_color(usage.ratio, cx);
        let segments = self
            .as_native_thread(cx)
            .map(|thread| {
                context_usage_segments(
                    &thread.read(cx).context_breakdown(cx),
                    usage.used_tokens,
                    cx,
                )
            })
            .unwrap_or_default();
        let bar = if segments.is_empty() || usage.max_tokens == 0 {
            ProgressBar::new("context-usage-bar", usage.ratio.min(1.0), 1.0, cx)
                .bg_color(colors.element_background)
                .fg_color(bar_color)
                .into_any_element()
        } else {
            // One continuous bar: the parts sit side by side with nearly square ends, the unused
            // rest of the window is the last part, and only the bar's two ends are rounded.
            // Rounding the container wouldn't do, since overflow clipping is rectangular.
            let end_radius = px(3.);
            h_flex()
                .w_full()
                .h(px(6.))
                .gap(px(2.))
                .children(segments.iter().enumerate().map(|(index, segment)| {
                    div()
                        .flex_none()
                        .h_full()
                        // Keeps a small part visible next to its legend row.
                        .min_w(px(3.))
                        .w(relative(segment.tokens as f32 / usage.max_tokens as f32))
                        .rounded(px(1.))
                        .when(index == 0, |this| this.rounded_l(end_radius))
                        .bg(segment.color)
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .rounded(px(1.))
                        .rounded_r(end_radius)
                        .bg(colors.element_background),
                )
                .into_any_element()
        };
        let legend = (!segments.is_empty()).then(|| {
            v_flex()
                .w_full()
                .gap_1p5()
                .pt_1()
                .children(segments.iter().map(|segment| {
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().size(px(12.)).rounded(px(3.)).bg(segment.color))
                                .child(Label::new(segment.label)),
                        )
                        .child(
                            Label::new(crate::humanize_token_count(segment.tokens))
                                .color(Color::Muted),
                        )
                }))
        });

        Some(
            composer_card(cx)
                .px_3p5()
                .py_3()
                .gap_2()
                .child(
                    h_flex()
                        .w_full()
                        .justify_between()
                        .child(Label::new("Context Usage").color(Color::Muted))
                        .child(
                            PillButton::new("hide-context-usage")
                                .size(KitSize::XSmall)
                                .icon(IconName::Dash)
                                .tooltip(Tooltip::text("Hide"))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.context_usage_expanded = false;
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_between()
                        .child(Label::new(format!("{} Full", usage.percentage)))
                        .child(
                            Label::new(format!("~{} / {} Tokens", usage.used, usage.max))
                                .color(Color::Muted),
                        ),
                )
                .child(bar)
                .children(legend)
                .into_any_element(),
        )
    }

    /// The Cursor account's usage, while the user is signed in to Cursor.
    fn cursor_usage(cx: &App) -> Option<cursor_agent::CursorUsage> {
        let state = cursor_agent::State::global(cx)?;
        let state = state.read(cx);
        if !state.is_authenticated() {
            return None;
        }
        state.usage().cloned()
    }

    fn refresh_cursor_usage(force: bool, cx: &mut App) {
        if let Some(state) = cursor_agent::State::global(cx) {
            state.update(cx, |state, cx| state.refresh_usage(force, cx));
        }
    }

    /// A Cursor turn spends from the account's allowance, so the reading is stale once it ends.
    pub(super) fn refresh_cursor_usage_after_turn(&self, cx: &mut Context<Self>) {
        let used_cursor = self.as_native_thread(cx).is_some_and(|thread| {
            thread
                .read(cx)
                .model()
                .is_some_and(|model| model.provider_id() == cursor_agent::PROVIDER_ID)
        });
        if used_cursor {
            Self::refresh_cursor_usage(true, cx);
        }
    }

    fn render_cursor_usage_ring(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let percent = Self::cursor_usage(cx)?.percent_used()?;
        let ratio = (percent / 100.0) as f32;
        let progress_color = Self::context_usage_color(ratio, cx);
        let percentage = format_usage_percent(percent);
        let tooltip_label = format!("Cursor Usage: {percentage}");

        Some(
            PillButton::new("cursor-usage-ring")
                .size(KitSize::Small)
                .toggle_state(self.cursor_usage_expanded)
                .child(
                    h_flex()
                        .gap_1()
                        .child(
                            CircularProgress::new(ratio, 1.0, px(16.), cx)
                                .stroke_width(px(2.))
                                .progress_color(progress_color),
                        )
                        .child(Label::new(percentage).size(LabelSize::Small)),
                )
                .tooltip(Tooltip::text(tooltip_label))
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.add_menu.is_some() {
                        this.close_add_menu(window, cx);
                    }
                    this.cursor_usage_expanded = !this.cursor_usage_expanded;
                    if this.cursor_usage_expanded {
                        this.context_usage_expanded = false;
                        Self::refresh_cursor_usage(false, cx);
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn render_cursor_usage_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let state = cursor_agent::State::global(cx)?;
        let (usage, usage_error, is_loading) = {
            let state = state.read(cx);
            if !state.is_authenticated() {
                return None;
            }
            (
                state.usage().cloned()?,
                state.usage_error(),
                state.is_loading_usage(),
            )
        };
        usage.percent_used()?;

        let resets = usage.billing_cycle_end.map(|billing_cycle_end| {
            format!(
                "Resets {}",
                chrono::DateTime::<chrono::Local>::from(billing_cycle_end).format("%b %-d")
            )
        });
        let mut metrics = Vec::new();
        if let Some(plan) = &usage.plan {
            let allowance = (plan.limit > 0.0).then(|| {
                let mut allowance = format!(
                    "{} / {}",
                    format_cents(plan.included_spend),
                    format_cents(plan.limit)
                );
                if plan.bonus_spend > 0.0 {
                    allowance.push_str(&format!(" (+{})", format_cents(plan.bonus_spend)));
                }
                allowance
            });
            metrics.push(
                cursor_usage_metric("total", "Total limit", resets, plan.total_percent_used, cx)
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .justify_between()
                            .child(
                                Label::new(format!(
                                    "{} used",
                                    format_usage_percent(plan.total_percent_used)
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            )
                            .children(allowance.map(|allowance| {
                                Label::new(allowance)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                            })),
                    ),
            );
            metrics.push(cursor_usage_metric(
                "api",
                "API limit",
                Some(format!(
                    "{} used",
                    format_usage_percent(plan.api_percent_used)
                )),
                plan.api_percent_used,
                cx,
            ));
            metrics.push(cursor_usage_metric(
                "auto",
                "Auto limit",
                Some(format!(
                    "{} used",
                    format_usage_percent(plan.auto_percent_used)
                )),
                plan.auto_percent_used,
                cx,
            ));
        }
        if let Some(spend_limit) = &usage.spend_limit {
            metrics.push(cursor_usage_metric(
                "spend-limit",
                "Spend limit",
                Some(format!(
                    "{} / {}",
                    format_cents(spend_limit.used),
                    format_cents(spend_limit.limit)
                )),
                spend_limit.percent_used(),
                cx,
            ));
        }

        Some(
            composer_card(cx)
                .px_3p5()
                .py_3()
                .gap_2p5()
                .child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_between()
                        .child(h_flex().gap_2().child(Label::new("Cursor Usage")).children(
                            usage.plan_name.map(|plan_name| {
                                Tag::secondary().with_size(KitSize::Small).child(plan_name)
                            }),
                        ))
                        .child(
                            h_flex()
                                .gap_0p5()
                                .child(
                                    PillButton::new("refresh-cursor-usage")
                                        .size(KitSize::XSmall)
                                        .icon(IconName::RotateCw)
                                        .disabled(is_loading)
                                        .tooltip(Tooltip::text("Refresh"))
                                        .on_click(|_, _, cx| Self::refresh_cursor_usage(true, cx)),
                                )
                                .child(
                                    PillButton::new("open-cursor-dashboard")
                                        .size(KitSize::XSmall)
                                        .icon(IconName::ArrowUpRight)
                                        .tooltip(Tooltip::text("Open Cursor Dashboard"))
                                        .on_click(|_, _, cx| {
                                            cx.open_url(CURSOR_USAGE_DASHBOARD_URL)
                                        }),
                                )
                                .child(
                                    PillButton::new("hide-cursor-usage")
                                        .size(KitSize::XSmall)
                                        .icon(IconName::Dash)
                                        .tooltip(Tooltip::text("Hide"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.cursor_usage_expanded = false;
                                            cx.notify();
                                        })),
                                ),
                        ),
                )
                .child(div().w_full().h_px().bg(cx.theme().colors().border_variant))
                .child(v_flex().w_full().gap_3().children(metrics))
                .children(
                    usage_error
                        .map(|error| Label::new(error).size(LabelSize::Small).color(Color::Error)),
                )
                .into_any_element(),
        )
    }
}

/// One limit in the Cursor usage card: its name and reading above a bar of the share used.
fn cursor_usage_metric(
    id: &'static str,
    label: &'static str,
    reading: Option<String>,
    percent: f64,
    cx: &App,
) -> Div {
    let theme = KitTheme::global(cx);
    let bar_color = if percent >= 95.0 {
        theme.danger
    } else if percent >= 80.0 {
        theme.warning
    } else {
        theme.blue_light
    };
    v_flex()
        .w_full()
        .gap_1p5()
        .child(
            h_flex()
                .w_full()
                .gap_2()
                .justify_between()
                .child(Label::new(label))
                .children(reading.map(|reading| Label::new(reading).color(Color::Muted))),
        )
        .child(
            ProgressBar::new(
                SharedString::from(format!("cursor-usage-{id}")),
                (percent / 100.0).clamp(0.0, 1.0) as f32,
                1.0,
                cx,
            )
            .bg_color(cx.theme().colors().element_background)
            .fg_color(bar_color),
        )
}

/// Small shares keep a decimal so a fresh cycle doesn't read as unused.
fn format_usage_percent(percent: f64) -> String {
    if percent > 0.0 && percent < 1.0 {
        format!("{percent:.1}%")
    } else {
        format!("{}%", percent.round() as u32)
    }
}

fn format_cents(cents: f64) -> String {
    let dollars = cents / 100.0;
    if (dollars - dollars.round()).abs() < 0.005 {
        format!("${}", dollars.round() as i64)
    } else {
        format!("${dollars:.2}")
    }
}

struct ContextUsageSegment {
    label: &'static str,
    tokens: u64,
    color: Hsla,
}

fn context_usage_segments(
    breakdown: &agent::ContextBreakdown,
    used_tokens: u64,
    cx: &App,
) -> Vec<ContextUsageSegment> {
    let theme = KitTheme::global(cx);
    let parts = [
        (
            "System prompt",
            breakdown.system_prompt,
            theme.muted_foreground,
        ),
        (
            "Tool definitions",
            breakdown.tool_definitions,
            theme.magenta,
        ),
        ("Rules", breakdown.rules, theme.green),
        ("Skills", breakdown.skills, theme.yellow),
        (
            "MCP & dynamic tools",
            breakdown.mcp_tools,
            theme.magenta_light,
        ),
        (
            "Subagent definitions",
            breakdown.subagent_definitions,
            theme.blue_light,
        ),
        (
            "Summarized conversation",
            breakdown.summarized_conversation,
            theme.red,
        ),
        ("Conversation", breakdown.conversation, theme.red_light),
    ];
    let estimated_total = parts.iter().map(|(_, tokens, _)| tokens).sum::<u64>();
    if estimated_total == 0 {
        return Vec::new();
    }

    // The parts are estimated from their size, so scale them to the count the model reported,
    // which keeps the bar and the legend in step with the total above them.
    parts
        .into_iter()
        .filter(|(_, tokens, _)| *tokens > 0)
        .map(|(label, tokens, color)| ContextUsageSegment {
            label,
            tokens: (u128::from(tokens) * u128::from(used_tokens) / u128::from(estimated_total))
                as u64,
            color,
        })
        .collect()
}

struct ContextUsage {
    ratio: f32,
    used_tokens: u64,
    max_tokens: u64,
    percentage: String,
    used: String,
    max: String,
}

/// The composer's fill and edge: a card a step lighter than the thread, edged like GPUI Kit's
/// inputs.
pub(super) fn composer_surface(cx: &App) -> (Hsla, Hsla) {
    let theme = KitTheme::global(cx);
    (theme.sidebar, theme.input)
}

fn composer_card(cx: &App) -> Div {
    let (background, border) = composer_surface(cx);
    v_flex()
        .w_full()
        .rounded(px(16.))
        .border_1()
        .border_color(border)
        .bg(background)
        .overflow_hidden()
}

/// Shows `card` below the composer without taking up room in its column, since a centered
/// composer would otherwise move whenever a menu opened.
fn float_below_composer(card: impl IntoElement) -> Div {
    div().absolute().top(relative(1.)).left_0().child(
        deferred(
            anchored()
                .snap_to_window_with_margin(px(8.))
                .child(div().pt_2().child(card)),
        )
        .with_priority(1),
    )
}

fn completions_menu_row(
    row: &CompletionsMenuRow,
    index: usize,
    selected: bool,
    editor: Entity<Editor>,
    cx: &App,
) -> AnyElement {
    // The list lays out each row on its own, where a row without a width shrinks to its content.
    match row {
        CompletionsMenuRow::Divider => h_flex()
            .w_full()
            .h_8()
            .px_2()
            .child(div().w_full().h_px().bg(cx.theme().colors().border_variant))
            .into_any_element(),
        CompletionsMenuRow::GroupHeader(label) => h_flex()
            .w_full()
            .h_8()
            .px_2()
            .child(
                Label::new(label.clone())
                    .size(LabelSize::Small)
                    .color(Color::Muted),
            )
            .into_any_element(),
        CompletionsMenuRow::Completion {
            label,
            match_positions,
            styled_ranges,
            detail,
            documentation,
            icon_path,
            icon_color,
        } => {
            // Mention and command labels style only their trailing location, such as a file's
            // directory or a skill's source, so that part reads as detail.
            let name_end = styled_ranges
                .first()
                .map(|range| range.start)
                .filter(|start| *start > 0)
                .unwrap_or(label.len());
            let (name, location) = match (label.get(..name_end), label.get(name_end..)) {
                (Some(name), Some(location)) => (name.trim_end(), location.trim()),
                _ => (label.as_ref(), ""),
            };
            let detail = [location, detail.as_deref().unwrap_or_default()]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            let match_positions = match_positions
                .iter()
                .copied()
                .filter(|position| *position < name.len())
                .collect();

            KitListItem::new(("composer-completion", index))
                .w_full()
                .h_8()
                .px_2()
                .rounded(px(6.))
                .selected(selected)
                .on_click(move |_, window, cx| {
                    editor.update(cx, |editor, cx| {
                        if let Some(task) = editor.confirm_completion(
                            &ConfirmCompletion {
                                item_ix: Some(index),
                            },
                            window,
                            cx,
                        ) {
                            task.detach_and_log_err(cx);
                        }
                    });
                })
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .children(icon_path.clone().map(|path| {
                            Icon::from_path(path)
                                .size(IconSize::Small)
                                .color(icon_color.map_or(Color::Muted, Color::Custom))
                        }))
                        .child(HighlightedLabel::new(name.to_string(), match_positions))
                        .when(!detail.is_empty(), |this| {
                            this.child(Label::new(detail).color(Color::Muted).truncate())
                        })
                        .children(documentation.clone().map(|documentation| {
                            div().ml_auto().min_w_0().child(
                                Label::new(documentation)
                                    .size(LabelSize::Small)
                                    .color(Color::Muted)
                                    .truncate(),
                            )
                        })),
                )
                .into_any_element()
        }
    }
}

fn agent_choice_icon(icon: &AgentChoiceIcon) -> Icon {
    match icon {
        AgentChoiceIcon::Builtin(icon) => Icon::new(*icon),
        AgentChoiceIcon::External(path) => Icon::from_external_svg(path.clone()),
    }
    .size(IconSize::Small)
    .color(Color::Muted)
}

fn agent_menu(
    mut menu: PopupMenu,
    choices: &[AgentChoice],
    current_agent: &Agent,
    panel: WeakEntity<AgentPanel>,
    conversation: WeakEntity<ConversationView>,
) -> PopupMenu {
    for choice in choices {
        let icon = choice.icon.clone();
        let label = choice.label.clone();
        let agent = choice.agent.clone();
        let panel = panel.clone();
        let conversation = conversation.clone();
        menu = menu.item(
            PopupMenuItem::element(move |_, _| {
                h_flex()
                    .gap_2()
                    .child(agent_choice_icon(&icon))
                    .child(Label::new(label.clone()))
            })
            .checked(choice.agent == *current_agent)
            .disabled(choice.disabled)
            .on_click(move |_, window, cx| {
                let agent = agent.clone();
                let panel = panel.clone();
                let conversation = conversation.clone();
                // The switch drops the thread this menu belongs to, so it waits for the click
                // to finish.
                window.defer(cx, move |window, cx| {
                    let Some(conversation) = conversation.upgrade() else {
                        return;
                    };
                    panel
                        .update(cx, |panel, cx| {
                            panel.switch_conversation_agent(conversation, agent, window, cx)
                        })
                        .log_err();
                });
            }),
        );
    }
    menu.separator().item(
        PopupMenuItem::new("Add More Agents")
            .icon(KitIcon::empty().path(IconName::Plus.path()))
            .on_click(|_, window, cx| {
                window.dispatch_action(Box::new(zed_actions::AcpRegistry), cx)
            }),
    )
}

/// A GPUI Kit button, pill shaped, with a tooltip of Zed's, since GPUI Kit's own
/// tooltips need its window root, which Zed's windows don't have. It can trigger a
/// [`PopoverMenu`].
#[derive(IntoElement)]
pub(super) struct PillButton {
    id: ElementId,
    variant: ButtonVariant,
    outline: bool,
    size: KitSize,
    icon: Option<IconName>,
    label: Option<SharedString>,
    children: Vec<AnyElement>,
    dropdown_caret: bool,
    selected: bool,
    disabled: bool,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView>>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
    debug_selector: Option<SharedString>,
    backdrop: Option<Hsla>,
}

impl PillButton {
    pub(super) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            variant: ButtonVariant::Ghost,
            outline: false,
            size: KitSize::Medium,
            icon: None,
            label: None,
            children: Vec::new(),
            dropdown_caret: false,
            selected: false,
            disabled: false,
            tooltip: None,
            on_click: None,
            debug_selector: None,
            backdrop: None,
        }
    }

    /// Paints `color` behind the button, for one that sits over content: GPUI Kit's outline
    /// variants are translucent, so whatever is behind would show through.
    pub(super) fn backdrop(mut self, color: Hsla) -> Self {
        self.backdrop = Some(color);
        self
    }

    /// Names the button for tests to find.
    pub(super) fn debug_selector(mut self, selector: impl Into<SharedString>) -> Self {
        self.debug_selector = Some(selector.into());
        self
    }

    pub(super) fn variant(mut self, variant: ButtonVariant) -> Self {
        self.variant = variant;
        self
    }

    pub(super) fn outline(mut self) -> Self {
        self.outline = true;
        self
    }

    pub(super) fn size(mut self, size: KitSize) -> Self {
        self.size = size;
        self
    }

    pub(super) fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(super) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub(super) fn dropdown_caret(mut self, dropdown_caret: bool) -> Self {
        self.dropdown_caret = dropdown_caret;
        self
    }

    pub(super) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(super) fn tooltip(
        mut self,
        tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        self.tooltip = Some(Box::new(tooltip));
        self
    }
}

impl ParentElement for PillButton {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Toggleable for PillButton {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }
}

impl Clickable for PillButton {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    fn cursor_style(self, _cursor_style: CursorStyle) -> Self {
        self
    }
}

impl RenderOnce for PillButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let height = match self.size {
            KitSize::Size(size) => size,
            KitSize::XSmall => px(20.),
            KitSize::Small => px(24.),
            KitSize::Medium | KitSize::Large => px(32.),
        };
        let button = KitButton::new(self.id.clone())
            .with_variant(self.variant)
            .when(self.outline, |button| button.outline())
            .with_size(self.size)
            .rounded(ButtonRounded::Size(height / 2.))
            .selected(self.selected)
            .disabled(self.disabled)
            .dropdown_caret(self.dropdown_caret)
            .when_some(self.icon, |button, icon| {
                button.icon(KitIcon::empty().path(icon.path()))
            })
            .when_some(self.label, |button, label| button.label(label))
            .children(self.children)
            .when_some(self.on_click, |button, on_click| {
                button.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_click(event, window, cx)
                })
            });
        div()
            .id(self.id)
            .flex_none()
            .when_some(self.backdrop, |this, color| {
                this.bg(color).rounded(height / 2.)
            })
            .when_some(self.debug_selector, |this, selector| {
                this.debug_selector(move || selector.to_string())
            })
            .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
            .child(button)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddMenuView {
    Root,
    McpServers,
}

#[derive(Clone)]
enum AddMenuItem {
    Mode(AgentProfileId),
    Follow,
    Files,
    Skill(AvailableSkill),
    Model,
    Open(AddMenuView),
    Back,
    McpServer(ContextServerId),
    ManageMcpServers,
}

#[derive(Clone)]
struct AddMenuEntry {
    item: AddMenuItem,
    label: SharedString,
    positions: Vec<usize>,
}

impl AddMenuEntry {
    fn new(item: AddMenuItem, label: impl Into<SharedString>) -> Self {
        Self {
            item,
            label: label.into(),
            positions: Vec::new(),
        }
    }
}

pub struct AddMenuDelegate {
    thread_view: WeakEntity<ThreadView>,
    message_editor: Entity<MessageEditor>,
    profile_selector: Option<Entity<ProfileSelector>>,
    context_server_store: Option<Entity<ContextServerStore>>,
    modes: Vec<(AgentProfileId, SharedString)>,
    skills: Vec<AvailableSkill>,
    model_name: Option<SharedString>,
    view: AddMenuView,
    sections: Vec<Vec<AddMenuEntry>>,
    selected_index: Option<IndexPath>,
    _store_subscription: Option<Subscription>,
}

impl AddMenuDelegate {
    fn mode_entries(&self) -> Vec<AddMenuEntry> {
        self.modes
            .iter()
            .map(|(profile_id, name)| {
                AddMenuEntry::new(AddMenuItem::Mode(profile_id.clone()), name.clone())
            })
            .collect()
    }

    fn follow_entry(&self) -> AddMenuEntry {
        AddMenuEntry::new(AddMenuItem::Follow, "Follow Agent")
    }

    fn is_following(&self, cx: &App) -> bool {
        self.thread_view
            .upgrade()
            .is_some_and(|thread_view| thread_view.read(cx).is_following(cx))
    }

    fn skill_entries(&self) -> Vec<AddMenuEntry> {
        self.skills
            .iter()
            .map(|skill| {
                AddMenuEntry::new(AddMenuItem::Skill(skill.clone()), skill.name.to_string())
            })
            .collect()
    }

    fn server_ids(&self, cx: &App) -> Vec<ContextServerId> {
        self.context_server_store
            .as_ref()
            .map(|store| store.read(cx).server_ids().to_vec())
            .unwrap_or_default()
    }

    fn server_entries(&self, cx: &App) -> Vec<AddMenuEntry> {
        self.server_ids(cx)
            .into_iter()
            .map(|server_id| {
                let label = server_id.0.to_string();
                AddMenuEntry::new(AddMenuItem::McpServer(server_id), label)
            })
            .collect()
    }

    fn root_entries(&self) -> Vec<AddMenuEntry> {
        let mut entries = vec![AddMenuEntry::new(AddMenuItem::Files, "Files")];
        if self.model_name.is_some() {
            entries.push(AddMenuEntry::new(AddMenuItem::Model, "Model"));
        }
        if self.context_server_store.is_some() {
            entries.push(AddMenuEntry::new(
                AddMenuItem::Open(AddMenuView::McpServers),
                "MCP",
            ));
        }
        entries
    }

    fn browse_groups(&self, cx: &App) -> Vec<Vec<AddMenuEntry>> {
        match self.view {
            AddMenuView::Root => vec![
                self.mode_entries(),
                vec![self.follow_entry()],
                self.root_entries(),
            ],
            AddMenuView::McpServers => vec![
                vec![AddMenuEntry::new(AddMenuItem::Back, "MCP")],
                self.server_entries(cx),
                vec![AddMenuEntry::new(
                    AddMenuItem::ManageMcpServers,
                    "Manage MCP Servers…",
                )],
            ],
        }
    }

    fn searchable_entries(&self, cx: &App) -> Vec<AddMenuEntry> {
        match self.view {
            AddMenuView::Root => {
                let mut entries = self.mode_entries();
                entries.push(self.follow_entry());
                entries.extend(self.root_entries());
                entries.extend(self.skill_entries());
                entries
            }
            AddMenuView::McpServers => self.server_entries(cx),
        }
    }

    fn show_browse_entries(&mut self, cx: &App) {
        self.sections = self
            .browse_groups(cx)
            .into_iter()
            .filter(|group| !group.is_empty())
            .collect();
    }

    fn entry(&self, index: IndexPath) -> Option<&AddMenuEntry> {
        self.sections.get(index.section)?.get(index.row)
    }

    fn first_selectable_index(&self, cx: &App) -> Option<IndexPath> {
        self.sections
            .iter()
            .enumerate()
            .find_map(|(section, entries)| {
                let row = entries.iter().position(|entry| {
                    !matches!(entry.item, AddMenuItem::Back) && self.is_enabled(&entry.item, cx)
                })?;
                Some(IndexPath::new(row).section(section))
            })
    }

    fn server_status(&self, server_id: &ContextServerId, cx: &App) -> ContextServerStatus {
        self.context_server_store
            .as_ref()
            .and_then(|store| store.read(cx).status_for_server(server_id))
            .unwrap_or(ContextServerStatus::Stopped)
    }

    fn is_enabled(&self, item: &AddMenuItem, cx: &App) -> bool {
        match item {
            AddMenuItem::McpServer(server_id) => !matches!(
                self.server_status(server_id, cx),
                ContextServerStatus::Starting | ContextServerStatus::Authenticating
            ),
            _ => true,
        }
    }

    fn active_profile_id(&self, cx: &App) -> Option<AgentProfileId> {
        self.profile_selector
            .as_ref()
            .map(|selector| selector.read(cx).active_profile_id(cx))
    }

    fn navigate(
        &mut self,
        view: AddMenuView,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.view = view;
        cx.defer_in(window, |list, window, cx| {
            list.set_query("", window, cx);
            let first_selectable = list.delegate().first_selectable_index(cx);
            list.set_selected_index(first_selectable, window, cx);
        });
    }

    fn dismiss(&self, window: &mut Window, cx: &mut App) {
        let thread_view = self.thread_view.clone();
        window.defer(cx, move |window, cx| {
            thread_view
                .update(cx, |thread_view, cx| thread_view.close_add_menu(window, cx))
                .log_err();
        });
    }

    fn toggle_server(&self, server_id: &ContextServerId, cx: &mut App) {
        let Some(store) = self.context_server_store.clone() else {
            return;
        };
        let enable = !store.read(cx).is_server_enabled(server_id, cx);

        store.update(cx, |store, cx| {
            if enable {
                if let Some(server) = store.get_server(server_id) {
                    store.start_server(server, cx);
                }
            } else {
                store.stop_server(server_id, cx).log_err();
            }
        });

        let fs = <dyn Fs>::global(cx);
        update_settings_file(fs, cx, {
            let server_id = server_id.clone();
            move |settings, _| {
                settings
                    .project
                    .context_servers
                    .entry(server_id.0.clone())
                    .or_insert_with(|| ContextServerSettingsContent::Extension {
                        enabled: enable,
                        remote: false,
                        settings: serde_json::json!({}),
                    })
                    .set_enabled(enable);
            }
        });
    }

    fn description(&self, item: &AddMenuItem, cx: &App) -> Option<SharedString> {
        match item {
            AddMenuItem::Mode(profile_id) => profile_description(profile_id).map(Into::into),
            AddMenuItem::Skill(skill) => {
                (!skill.description.is_empty()).then(|| skill.description.to_string().into())
            }
            AddMenuItem::Model => self.model_name.clone(),
            AddMenuItem::McpServer(server_id) => {
                let status = match self.server_status(server_id, cx) {
                    ContextServerStatus::Running => "Running",
                    ContextServerStatus::Starting => "Starting…",
                    ContextServerStatus::Authenticating => "Authenticating…",
                    ContextServerStatus::Stopped => "Off",
                    ContextServerStatus::Error(_) => "Error",
                    ContextServerStatus::AuthRequired
                    | ContextServerStatus::ClientSecretRequired { .. } => "Needs sign-in",
                };
                Some(status.into())
            }
            _ => None,
        }
    }

    fn render_end_slot(&self, item: &AddMenuItem, cx: &App) -> Option<AnyElement> {
        match item {
            AddMenuItem::Mode(profile_id) => {
                (self.active_profile_id(cx).as_ref() == Some(profile_id)).then(|| {
                    Icon::new(IconName::Check)
                        .size(IconSize::Small)
                        .color(Color::Muted)
                        .into_any_element()
                })
            }
            AddMenuItem::Follow => Some(
                KitSwitch::new("composer-follow-toggle")
                    .checked(self.is_following(cx))
                    .with_size(KitSize::Small)
                    .into_any_element(),
            ),
            AddMenuItem::Open(_) => Some(
                Icon::new(IconName::ChevronRight)
                    .size(IconSize::Small)
                    .color(Color::Muted)
                    .into_any_element(),
            ),
            AddMenuItem::McpServer(server_id) => {
                let is_enabled = self
                    .context_server_store
                    .as_ref()
                    .is_some_and(|store| store.read(cx).is_server_enabled(server_id, cx));
                Some(
                    KitSwitch::new(SharedString::from(format!(
                        "composer-mcp-toggle-{}",
                        server_id.0
                    )))
                    .checked(is_enabled)
                    .with_size(KitSize::Small)
                    .into_any_element(),
                )
            }
            _ => None,
        }
    }

    fn item_icon(&self, item: &AddMenuItem, cx: &App) -> (IconName, Color) {
        match item {
            AddMenuItem::Mode(profile_id) => profile_icon(profile_id),
            AddMenuItem::Follow => (IconName::Crosshair, Color::Muted),
            AddMenuItem::Files => (IconName::Paperclip, Color::Muted),
            AddMenuItem::Skill(_) => (IconName::Sparkle, Color::Muted),
            AddMenuItem::Model => (IconName::Blocks, Color::Muted),
            AddMenuItem::Open(_) | AddMenuItem::ManageMcpServers => {
                (IconName::ToolHammer, Color::Muted)
            }
            AddMenuItem::Back => (IconName::ChevronLeft, Color::Muted),
            AddMenuItem::McpServer(server_id) => {
                let color = match self.server_status(server_id, cx) {
                    ContextServerStatus::Running => Color::Success,
                    ContextServerStatus::Starting | ContextServerStatus::Authenticating => {
                        Color::Warning
                    }
                    ContextServerStatus::Stopped => Color::Disabled,
                    ContextServerStatus::Error(_)
                    | ContextServerStatus::AuthRequired
                    | ContextServerStatus::ClientSecretRequired { .. } => Color::Error,
                };
                (IconName::Circle, color)
            }
        }
    }
}

impl ListDelegate for AddMenuDelegate {
    type Item = KitListItem;

    fn perform_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.show_browse_entries(cx);
            cx.notify();
            return Task::ready(());
        }

        let searchable = self.searchable_entries(cx);
        let candidates = searchable
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatchCandidate::new(index, &entry.label))
            .collect::<Vec<_>>();
        let executor = cx.background_executor().clone();

        cx.spawn_in(window, async move |list, cx| {
            let cancel_flag = AtomicBool::new(false);
            let matches = match_strings(
                &candidates,
                &query,
                false,
                true,
                100,
                &cancel_flag,
                executor,
            )
            .await;

            list.update_in(cx, |list, window, cx| {
                let entries = matches
                    .into_iter()
                    .filter_map(|string_match| {
                        let entry = searchable.get(string_match.candidate_id)?;
                        Some(AddMenuEntry {
                            positions: string_match.positions,
                            ..entry.clone()
                        })
                    })
                    .collect::<Vec<_>>();
                let delegate = list.delegate_mut();
                delegate.sections = if entries.is_empty() {
                    Vec::new()
                } else {
                    vec![entries]
                };
                let first_selectable = delegate.first_selectable_index(cx);
                list.set_selected_index(first_selectable, window, cx);
                cx.notify();
            })
            .log_err();
        })
    }

    fn sections_count(&self, _: &App) -> usize {
        self.sections.len()
    }

    fn items_count(&self, section: usize, _: &App) -> usize {
        self.sections.get(section).map_or(0, Vec::len)
    }

    fn set_selected_index(
        &mut self,
        index: Option<IndexPath>,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        self.selected_index = index;
        cx.notify();
    }

    fn confirm(
        &mut self,
        _secondary: bool,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        let Some(entry) = self
            .selected_index
            .and_then(|index| self.entry(index))
            .cloned()
        else {
            return;
        };
        if !self.is_enabled(&entry.item, cx) {
            return;
        }

        match entry.item {
            AddMenuItem::Mode(profile_id) => {
                if let Some(profile_selector) = self.profile_selector.as_ref() {
                    profile_selector.update(cx, |selector, cx| {
                        selector.select_profile(profile_id, cx);
                        cx.notify();
                    });
                }
                self.dismiss(window, cx);
            }
            AddMenuItem::Follow => {
                self.thread_view
                    .update(cx, |thread_view, cx| {
                        thread_view.toggle_following(window, cx);
                        cx.notify();
                    })
                    .log_err();
                cx.notify();
            }
            AddMenuItem::Files => {
                let message_editor = self.message_editor.clone();
                self.dismiss(window, cx);
                window.defer(cx, move |window, cx| {
                    message_editor.focus_handle(cx).focus(window, cx);
                    message_editor.update(cx, |editor, cx| {
                        editor.insert_context_type("file", window, cx);
                    });
                });
            }
            AddMenuItem::Skill(skill) => {
                let message_editor = self.message_editor.clone();
                self.dismiss(window, cx);
                window.defer(cx, move |window, cx| {
                    message_editor.focus_handle(cx).focus(window, cx);
                    message_editor.update(cx, |editor, cx| {
                        editor.insert_skill_crease(&skill, window, cx);
                    });
                });
            }
            AddMenuItem::Model => {
                let message_editor = self.message_editor.clone();
                self.dismiss(window, cx);
                window.defer(cx, move |window, cx| {
                    message_editor.focus_handle(cx).focus(window, cx);
                    window.dispatch_action(ToggleModelSelector.boxed_clone(), cx);
                });
            }
            AddMenuItem::Open(view) => self.navigate(view, window, cx),
            AddMenuItem::Back => self.navigate(AddMenuView::Root, window, cx),
            AddMenuItem::McpServer(server_id) => {
                self.toggle_server(&server_id, cx);
                cx.notify();
            }
            AddMenuItem::ManageMcpServers => {
                self.dismiss(window, cx);
                window.dispatch_action(
                    Box::new(zed_actions::OpenSettingsAt {
                        path: "context_servers".to_string(),
                        target: None,
                    }),
                    cx,
                );
            }
        }
    }

    // The list sizes every header like the first one, so the first section gets an empty
    // header of the same height rather than none.
    fn render_section_header(
        &mut self,
        section: usize,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let border = cx.theme().colors().border_variant;
        Some(h_flex().h(px(9.)).px_2().when(section > 0, |this| {
            this.child(div().w_full().h_px().bg(border))
        }))
    }

    fn render_empty(
        &mut self,
        _: &mut Window,
        _: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        h_flex()
            .h_9()
            .px_3()
            .child(Label::new("No matches").color(Color::Muted))
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<KitListItem> {
        let entry = self.entry(index)?;
        let enabled = self.is_enabled(&entry.item, cx);
        let is_back = matches!(entry.item, AddMenuItem::Back);
        let (icon, icon_color) = self.item_icon(&entry.item, cx);
        let label_color = if !enabled {
            Color::Disabled
        } else if is_back {
            Color::Muted
        } else {
            Color::Default
        };
        let icon_size = if matches!(entry.item, AddMenuItem::McpServer(_)) {
            IconSize::XSmall
        } else {
            IconSize::Small
        };

        let debug_selector = format!("composer-add-menu-{}", entry.label);
        Some(
            KitListItem::new("composer-add-menu-item")
                .h_8()
                .px_2()
                .rounded(px(6.))
                .disabled(!enabled)
                .accessibility_label(entry.label.clone())
                .child(
                    h_flex()
                        .debug_selector(move || debug_selector)
                        .w_full()
                        .min_w_0()
                        .gap_2()
                        .child(Icon::new(icon).size(icon_size).color(if enabled {
                            icon_color
                        } else {
                            Color::Disabled
                        }))
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(
                                    HighlightedLabel::new(
                                        entry.label.clone(),
                                        entry.positions.clone(),
                                    )
                                    .color(label_color),
                                )
                                .children(self.description(&entry.item, cx).map(|description| {
                                    Label::new(description).color(Color::Muted).truncate()
                                })),
                        )
                        .children(self.render_end_slot(&entry.item, cx)),
                ),
        )
    }
}
