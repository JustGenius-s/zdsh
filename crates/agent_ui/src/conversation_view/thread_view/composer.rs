use super::*;
use crate::profile_selector::{profile_description, profile_icon};
use agent_settings::{AgentProfile, builtin_profiles};
use context_server::ContextServerId;
use fuzzy::{StringMatchCandidate, match_strings};
use gpui::{AnyView, DismissEvent, MouseButton, canvas};
use picker::{ErasedEditor, Picker, PickerDelegate};
use project::context_server_store::{ContextServerStatus, ContextServerStore};
use settings::ContextServerSettingsContent;
use std::sync::atomic::AtomicBool;
use ui::{HighlightedLabel, ListItem, ProgressBar, Switch};

const FOLLOW_UP_PLACEHOLDER: &str = "Send follow-up";

impl ThreadView {
    pub(crate) fn render_message_editor(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if self.is_subagent() {
            return div().into_any_element();
        }

        let editor_expanded = self.editor_expanded;
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
        self.sync_add_menu_width(window, cx);

        let context_usage_card = self
            .context_usage_expanded
            .then(|| self.render_context_usage_card(cx))
            .flatten();
        let add_menu_card = self.add_menu.clone().map(|picker| {
            composer_card(cx)
                .py_1()
                // Picker rows aren't focusable, so a press on one would otherwise
                // hand focus to the thread view, blur the menu and close it before
                // the click lands.
                .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
                .child(picker)
        });
        let changes_chips = self.render_changes_chips(cx);
        let composer = self.render_composer_pill(cx);
        let footer = self.render_composer_footer(cx);
        let composer_bounds = self.composer_bounds.clone();

        v_flex()
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
                    .when(self.add_menu.is_some(), |this| {
                        this.on_mouse_down_out(cx.listener(|this, _, window, cx| {
                            this.close_add_menu(window, cx);
                        }))
                    })
                    .children(context_usage_card)
                    .children(add_menu_card)
                    .children(changes_chips)
                    .child(composer)
                    .child(footer),
            )
            .into_any()
    }

    fn render_composer_pill(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    Button::new("discard-protected-draft", "Discard draft")
                        .label_size(LabelSize::Small)
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
            IconButton::new("toggle-height", expand_icon)
                .icon_size(IconSize::Small)
                .icon_color(Color::Muted)
                .tooltip(move |_window, cx| {
                    Tooltip::for_action_in(expand_tooltip, &ExpandMessageEditor, &focus_handle, cx)
                })
                .on_click(cx.listener(|this, _, window, cx| {
                    this.expand_message_editor(&ExpandMessageEditor, window, cx);
                }))
        });

        let trailing_controls = h_flex()
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
            .child(self.render_send_button(cx));

        let colors = cx.theme().colors();
        h_flex()
            .w_full()
            .min_w_0()
            .when(editor_expanded, |this| this.flex_1().min_h_0())
            .items_end()
            .gap_1p5()
            .p(px(6.))
            .rounded(px(22.))
            .border_1()
            .border_color(colors.border_variant)
            .bg(colors.elevated_surface_background)
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
            .child(trailing_controls)
            .into_any_element()
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

        let colors = cx.theme().colors();
        let filled_background = colors.text;
        let filled_foreground = colors.elevated_surface_background;

        if is_generating && is_editor_empty {
            return ComposerPill::new("stop-generation")
                .circle(px(30.))
                .style(PillStyle::Custom(filled_background))
                .child(
                    Icon::new(IconName::Stop)
                        .size(IconSize::XSmall)
                        .color(Color::Custom(filled_foreground)),
                )
                .tooltip(move |_window, cx| {
                    Tooltip::for_action("Stop Generation", &editor::actions::Cancel, cx)
                })
                .on_click(cx.listener(|this, _event, _, cx| this.cancel_generation(cx)))
                .into_any_element();
        }

        let can_send = !is_editor_empty || is_generating;
        let background = if can_send {
            filled_background
        } else {
            filled_background.opacity(0.3)
        };

        ComposerPill::new("send-message")
            .circle(px(30.))
            .style(PillStyle::Custom(background))
            .disabled(!can_send)
            .child(
                Icon::new(IconName::ArrowUp)
                    .size(IconSize::Small)
                    .color(Color::Custom(filled_foreground)),
            )
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

    fn render_add_menu_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let focus_handle = self.message_editor.focus_handle(cx);

        div()
            .debug_selector(|| "composer-add-context".into())
            .child(
                ComposerPill::new("add-context")
                    .circle(px(30.))
                    .toggle_state(self.add_menu.is_some())
                    .child(
                        Icon::new(IconName::Plus)
                            .size(IconSize::Small)
                            .color(Color::Muted),
                    )
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

    /// Mirrors Cursor: the default mode stays implicit, any other mode is shown
    /// next to the add button so it is obvious the agent behaves differently.
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

        Some(
            ComposerPill::new("composer-mode")
                .height(px(30.))
                .style(PillStyle::Subtle)
                .child(Icon::new(icon).size(IconSize::Small).color(color))
                .child(Label::new(name).color(color))
                .tooltip(Tooltip::text("Change Mode"))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.toggle_add_menu(window, cx);
                }))
                .into_any_element(),
        )
    }

    pub(super) fn toggle_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_menu.is_some() {
            self.close_add_menu(window, cx);
            return;
        }

        let picker = self.build_add_menu(window, cx);
        let picker_focus = picker.focus_handle(cx);
        self._add_menu_subscriptions = vec![
            cx.subscribe_in(&picker, window, |this, _, _: &DismissEvent, window, cx| {
                this.close_add_menu(window, cx);
            }),
            cx.on_blur(&picker_focus, window, |this, window, cx| {
                this.close_add_menu(window, cx);
            }),
        ];
        picker_focus.focus(window, cx);
        self.add_menu = Some(picker);
        self.context_usage_expanded = false;
        cx.notify();
    }

    fn close_add_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(picker) = self.add_menu.take() else {
            return;
        };
        self.add_menu_width = None;
        self._add_menu_subscriptions.clear();
        // Only reclaim focus when the menu had it; a click elsewhere already
        // moved focus where the user wants it.
        if picker.focus_handle(cx).is_focused(window) {
            self.message_editor.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn add_menu_width_for(&self) -> Option<Pixels> {
        // The card's 1px border sits inside the composer column.
        self.composer_bounds
            .get()
            .map(|bounds| bounds.size.width - px(2.))
    }

    fn sync_add_menu_width(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(picker) = self.add_menu.clone() else {
            return;
        };
        let Some(width) = self.add_menu_width_for() else {
            return;
        };
        if self.add_menu_width == Some(width) {
            return;
        }
        self.add_menu_width = Some(width);
        let width = rems(width / window.rem_size());
        picker.update(cx, |picker, cx| picker.set_width(width, cx));
    }

    fn build_add_menu(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Picker<AddMenuDelegate>> {
        let session_capabilities = self.session_capabilities.read();
        let supports_images = session_capabilities.supports_images();
        let supports_embedded_context = session_capabilities.supports_embedded_context();
        let skills = session_capabilities.completion_skills();
        drop(session_capabilities);

        let has_selection = self.has_selection_to_add(cx);
        let modes = self
            .profile_selector
            .as_ref()
            .filter(|selector| selector.read(cx).profiles_available(cx))
            .map(|_| {
                AgentProfile::available_profiles(cx)
                    .into_iter()
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
            message_editor: self.message_editor.clone(),
            profile_selector: self.profile_selector.clone(),
            context_server_store: context_server_store.clone(),
            modes,
            skills,
            supports_images,
            supports_embedded_context,
            has_selection,
            model_name,
            view: AddMenuView::Root,
            entries: Vec::new(),
            separators: Vec::new(),
            selected_index: 0,
            _store_subscription: None,
        };
        delegate.show_browse_entries(cx);

        let width = self.add_menu_width_for().unwrap_or(px(320.));
        self.add_menu_width = Some(width);
        let width = rems(width / window.rem_size());

        cx.new(|cx| {
            let mut picker = Picker::list(delegate, window, cx)
                .embedded()
                .show_scrollbar(true)
                .initial_width(width)
                .max_height(rems(24.));
            if let Some(store) = context_server_store {
                picker.delegate._store_subscription =
                    Some(cx.observe(&store, |_, _, cx| cx.notify()));
            }
            picker
        })
    }

    fn has_selection_to_add(&self, cx: &mut Context<Self>) -> bool {
        let has_editor_selection = self
            .workspace
            .upgrade()
            .and_then(|workspace| {
                workspace
                    .read(cx)
                    .active_item(cx)
                    .and_then(|item| item.downcast::<Editor>())
            })
            .is_some_and(|editor| {
                editor.update(cx, |editor, cx| {
                    editor.has_non_empty_selection(&editor.display_snapshot(cx))
                })
            });

        has_editor_selection
            || self.workspace.upgrade().is_some_and(|workspace| {
                workspace
                    .read(cx)
                    .items_of_type::<terminal_view::TerminalView>(cx)
                    .any(|view| {
                        view.read(cx)
                            .terminal()
                            .read(cx)
                            .last_content
                            .selection_text
                            .as_ref()
                            .is_some_and(|text| !text.is_empty())
                    })
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

            ComposerPill::new("composer-changes")
                .height(px(28.))
                .style(PillStyle::Outlined)
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
            PopoverMenu::new("composer-commit-menu")
                .trigger(
                    ComposerPill::new("composer-commit")
                        .height(px(28.))
                        .style(PillStyle::Outlined)
                        .child(Label::new("Commit"))
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
                .anchor(gpui::Anchor::BottomLeft)
                .offset(gpui::Point {
                    x: px(0.0),
                    y: px(-4.0),
                })
                .menu(|window, cx| {
                    Some(ContextMenu::build(window, cx, |menu, _window, _cx| {
                        menu.action("Commit…", git::Commit.boxed_clone())
                            .action("Amend Last Commit…", git::Amend.boxed_clone())
                            .separator()
                            .action("Push", git::Push.boxed_clone())
                    }))
                })
        });

        Some(
            h_flex()
                .w_full()
                .gap_1p5()
                .children(changes_chip)
                .children(commit_chip)
                .into_any_element(),
        )
    }

    fn render_composer_footer(&self, cx: &mut Context<Self>) -> AnyElement {
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
                    .child(self.render_follow_toggle(cx))
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
                .trigger_with_tooltip(
                    Button::new("composer-branch", branch_name)
                        .color(Color::Muted)
                        .truncate(true)
                        .start_icon(
                            Icon::new(IconName::GitBranch)
                                .size(IconSize::Small)
                                .color(Color::Muted),
                        ),
                    move |_window, cx| {
                        Tooltip::with_meta(
                            "Branch & Stash",
                            Some(&zed_actions::git::Branch),
                            format!("Currently Checked Out: {}", tooltip_label),
                            cx,
                        )
                    },
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

        let cost_label = thread.cost().map(|cost| {
            let precision = if cost.amount > 0.0 && cost.amount < 0.01 {
                4
            } else {
                2
            };
            format!("{:.prec$} {}", cost.amount, cost.currency, prec = precision)
        });

        let native_thread = self.as_native_thread(cx);
        let split = self.supports_split_token_display(cx).then(|| {
            let (max_input_tokens, max_output_tokens) = native_thread
                .as_ref()
                .map(|thread| {
                    let thread = thread.read(cx);
                    (
                        thread.input_token_capacity().unwrap_or(usage.max_tokens),
                        thread
                            .model()
                            .and_then(|model| model.max_output_tokens())
                            .unwrap_or(0),
                    )
                })
                .unwrap_or((usage.max_tokens, 0));
            SplitTokenUsage {
                input: format!(
                    "{} / {}",
                    crate::humanize_token_count(usage.input_tokens),
                    crate::humanize_token_count(max_input_tokens)
                ),
                output: format!(
                    "{} / {}",
                    crate::humanize_token_count(usage.output_tokens),
                    crate::humanize_token_count(max_output_tokens)
                ),
            }
        });

        let project_rules_entry_ids = native_thread
            .map(|thread| {
                thread
                    .read(cx)
                    .project_context()
                    .read(cx)
                    .worktrees
                    .iter()
                    .filter_map(|worktree| worktree.rules_file.as_ref())
                    .map(|rules_file| ProjectEntryId::from_usize(rules_file.project_entry_id))
                    .collect()
            })
            .unwrap_or_default();

        Some(ContextUsage {
            ratio,
            percentage: format!("{}%", (ratio * 100.0).round() as u32),
            used: crate::humanize_token_count(usage.used_tokens),
            max: crate::humanize_token_count(usage.max_tokens),
            split,
            cost_label,
            global_rules_loaded: UserAgentsMd::global(cx)
                .and_then(|agents_md| agents_md.content())
                .is_some(),
            project_rules_entry_ids,
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
            ComposerPill::new("context-usage-ring")
                .circle(px(26.))
                .style(PillStyle::Subtle)
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
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    fn render_context_usage_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let usage = self.context_usage(cx)?;
        let colors = cx.theme().colors();
        let bar_color = Self::context_usage_color(usage.ratio, cx);

        let detail_row = |label: &'static str, value: String| {
            h_flex()
                .w_full()
                .gap_2()
                .justify_between()
                .child(Label::new(label).color(Color::Muted))
                .child(Label::new(value).color(Color::Muted))
        };

        let rules_row = |id: &'static str, label: &'static str, value: String| {
            h_flex()
                .id(id)
                .w_full()
                .gap_2()
                .justify_between()
                .cursor_pointer()
                .child(Label::new(label).color(Color::Muted))
                .child(
                    h_flex()
                        .gap_1()
                        .child(Label::new(value).color(Color::Muted))
                        .child(
                            Icon::new(IconName::ArrowUpRight)
                                .size(IconSize::XSmall)
                                .color(Color::Muted),
                        ),
                )
        };

        let global_rules_row = usage.global_rules_loaded.then(|| {
            let workspace = self.workspace.clone();
            rules_row("open-global-rules", "Global rules", "1 file".to_string()).on_click(
                move |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace
                                .open_abs_path(
                                    paths::agents_file().clone(),
                                    workspace::OpenOptions {
                                        focus: Some(true),
                                        ..Default::default()
                                    },
                                    window,
                                    cx,
                                )
                                .detach_and_log_err(cx);
                        })
                        .log_err();
                },
            )
        });

        let project_rules_count = usage.project_rules_entry_ids.len();
        let project_rules_row = (project_rules_count > 0).then(|| {
            let workspace = self.workspace.clone();
            let project_rules_entry_ids = usage.project_rules_entry_ids.clone();
            rules_row(
                "open-project-rules",
                "Project rules",
                format!(
                    "{} {}",
                    project_rules_count,
                    pluralize("file", project_rules_count)
                ),
            )
            .on_click(move |_, window, cx| {
                workspace
                    .update(cx, |workspace, cx| {
                        let project = workspace.project().read(cx);
                        let paths = project_rules_entry_ids
                            .iter()
                            .flat_map(|entry_id| project.path_for_entry(*entry_id, cx))
                            .collect::<Vec<_>>();
                        for path in paths {
                            workspace
                                .open_path(path, None, true, window, cx)
                                .detach_and_log_err(cx);
                        }
                    })
                    .log_err();
            })
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
                            IconButton::new("hide-context-usage", IconName::Dash)
                                .icon_size(IconSize::Small)
                                .icon_color(Color::Muted)
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
                .child(
                    ProgressBar::new("context-usage-bar", usage.ratio.min(1.0), 1.0, cx)
                        .bg_color(colors.element_background)
                        .fg_color(bar_color),
                )
                .when_some(usage.split, |this, split| {
                    this.child(detail_row("Input", split.input))
                        .child(detail_row("Output", split.output))
                })
                .when_some(usage.cost_label, |this, cost_label| {
                    this.child(detail_row("Cost", cost_label))
                })
                .children(global_rules_row)
                .children(project_rules_row)
                .into_any_element(),
        )
    }
}

struct SplitTokenUsage {
    input: String,
    output: String,
}

struct ContextUsage {
    ratio: f32,
    percentage: String,
    used: String,
    max: String,
    split: Option<SplitTokenUsage>,
    cost_label: Option<String>,
    global_rules_loaded: bool,
    project_rules_entry_ids: Vec<ProjectEntryId>,
}

fn composer_card(cx: &App) -> Div {
    let colors = cx.theme().colors();
    v_flex()
        .w_full()
        .rounded(px(16.))
        .border_1()
        .border_color(colors.border_variant)
        .bg(colors.elevated_surface_background)
        .overflow_hidden()
}

#[derive(Clone, Copy)]
enum PillStyle {
    Filled,
    Subtle,
    Outlined,
    /// A fixed fill (the send button) that dims on hover instead of recoloring.
    Custom(Hsla),
}

#[derive(IntoElement)]
struct ComposerPill {
    id: ElementId,
    children: Vec<AnyElement>,
    circle_size: Option<Pixels>,
    height: Pixels,
    style: PillStyle,
    toggled: bool,
    disabled: bool,
    cursor_style: CursorStyle,
    tooltip: Option<Box<dyn Fn(&mut Window, &mut App) -> AnyView>>,
    on_click: Option<Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>>,
}

impl ComposerPill {
    fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            children: Vec::new(),
            circle_size: None,
            height: px(22.),
            style: PillStyle::Filled,
            toggled: false,
            disabled: false,
            cursor_style: CursorStyle::PointingHand,
            tooltip: None,
            on_click: None,
        }
    }

    fn circle(mut self, size: Pixels) -> Self {
        self.circle_size = Some(size);
        self
    }

    fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    fn style(mut self, style: PillStyle) -> Self {
        self.style = style;
        self
    }

    fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    fn tooltip(mut self, tooltip: impl Fn(&mut Window, &mut App) -> AnyView + 'static) -> Self {
        self.tooltip = Some(Box::new(tooltip));
        self
    }
}

impl ParentElement for ComposerPill {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl Toggleable for ComposerPill {
    fn toggle_state(mut self, selected: bool) -> Self {
        self.toggled = selected;
        self
    }
}

impl Clickable for ComposerPill {
    fn on_click(mut self, handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }

    fn cursor_style(mut self, cursor_style: CursorStyle) -> Self {
        self.cursor_style = cursor_style;
        self
    }
}

impl RenderOnce for ComposerPill {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let colors = cx.theme().colors();
        let enabled = !self.disabled;
        let selected_background = colors.ghost_element_selected;
        let (background, hover_background) = match self.style {
            PillStyle::Filled => (
                Some(if self.toggled {
                    selected_background
                } else {
                    colors.element_background
                }),
                Some(colors.element_hover),
            ),
            PillStyle::Subtle | PillStyle::Outlined => (
                self.toggled.then_some(selected_background),
                Some(colors.ghost_element_hover),
            ),
            PillStyle::Custom(fill) => (Some(fill), None),
        };

        h_flex()
            .id(self.id)
            .flex_none()
            .items_center()
            .justify_center()
            .gap_1()
            .rounded_full()
            .map(|this| match self.circle_size {
                Some(size) => this.size(size),
                None => this.h(self.height).px_2p5(),
            })
            .when(matches!(self.style, PillStyle::Outlined), |this| {
                this.border_1().border_color(colors.border_variant)
            })
            .when_some(background, |this, background| this.bg(background))
            .when(enabled, |this| {
                this.cursor(self.cursor_style)
                    .hover(move |style| match hover_background {
                        Some(hover_background) => style.bg(hover_background),
                        None => style.opacity(0.85),
                    })
                    // Like `ButtonLike`, keep focus where it is (the composer or an
                    // open menu) instead of moving it to the thread view.
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            })
            .when_some(self.tooltip, |this, tooltip| this.tooltip(tooltip))
            .when_some(self.on_click.filter(|_| enabled), |this, on_click| {
                this.on_click(move |event, window, cx| {
                    cx.stop_propagation();
                    on_click(event, window, cx)
                })
            })
            .children(self.children)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AddMenuView {
    Root,
    Skills,
    McpServers,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ContextItem {
    Files,
    Symbols,
    Threads,
    Image,
    Selection,
    BranchDiff,
}

impl ContextItem {
    const ALL: [Self; 6] = [
        Self::Files,
        Self::Symbols,
        Self::Threads,
        Self::Image,
        Self::Selection,
        Self::BranchDiff,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Symbols => "Symbols",
            Self::Threads => "Threads",
            Self::Image => "Image",
            Self::Selection => "Selection",
            Self::BranchDiff => "Branch Diff",
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Files => IconName::Paperclip,
            Self::Symbols => IconName::Code,
            Self::Threads => IconName::Thread,
            Self::Image => IconName::Image,
            Self::Selection => IconName::CursorIBeam,
            Self::BranchDiff => IconName::GitBranch,
        }
    }

    fn apply(self, message_editor: &Entity<MessageEditor>, window: &mut Window, cx: &mut App) {
        message_editor.focus_handle(cx).focus(window, cx);
        match self {
            Self::Files => message_editor.update(cx, |editor, cx| {
                editor.insert_context_type("file", window, cx);
            }),
            Self::Symbols => message_editor.update(cx, |editor, cx| {
                editor.insert_context_type("symbol", window, cx);
            }),
            Self::Threads => message_editor.update(cx, |editor, cx| {
                editor.insert_context_type("thread", window, cx);
            }),
            Self::Image => message_editor.update(cx, |editor, cx| {
                editor.add_images_from_picker(window, cx);
            }),
            Self::Selection => {
                window.dispatch_action(zed_actions::agent::AddSelectionToThread.boxed_clone(), cx)
            }
            Self::BranchDiff => message_editor.update(cx, |editor, cx| {
                editor.insert_branch_diff_crease(window, cx);
            }),
        }
    }
}

#[derive(Clone)]
enum AddMenuItem {
    Mode(AgentProfileId),
    Context(ContextItem),
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
    message_editor: Entity<MessageEditor>,
    profile_selector: Option<Entity<ProfileSelector>>,
    context_server_store: Option<Entity<ContextServerStore>>,
    modes: Vec<(AgentProfileId, SharedString)>,
    skills: Vec<AvailableSkill>,
    supports_images: bool,
    supports_embedded_context: bool,
    has_selection: bool,
    model_name: Option<SharedString>,
    view: AddMenuView,
    entries: Vec<AddMenuEntry>,
    separators: Vec<usize>,
    selected_index: usize,
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

    fn context_entries(&self) -> Vec<AddMenuEntry> {
        ContextItem::ALL
            .into_iter()
            .map(|item| AddMenuEntry::new(AddMenuItem::Context(item), item.label()))
            .collect()
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

    fn navigation_entries(&self) -> Vec<AddMenuEntry> {
        let mut entries = Vec::new();
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
            AddMenuView::Root => {
                let mut context = self.context_entries();
                if !self.skills.is_empty() {
                    context.push(AddMenuEntry::new(
                        AddMenuItem::Open(AddMenuView::Skills),
                        "Skills",
                    ));
                }
                context.extend(self.navigation_entries());
                vec![self.mode_entries(), context]
            }
            AddMenuView::Skills => vec![
                vec![AddMenuEntry::new(AddMenuItem::Back, "Skills")],
                self.skill_entries(),
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
                entries.extend(self.context_entries());
                entries.extend(self.skill_entries());
                entries.extend(self.navigation_entries());
                entries
            }
            AddMenuView::Skills => self.skill_entries(),
            AddMenuView::McpServers => self.server_entries(cx),
        }
    }

    fn show_browse_entries(&mut self, cx: &App) {
        self.entries.clear();
        self.separators.clear();
        for group in self.browse_groups(cx) {
            if group.is_empty() {
                continue;
            }
            if !self.entries.is_empty() {
                self.separators.push(self.entries.len() - 1);
            }
            self.entries.extend(group);
        }
        self.selected_index = self.first_selectable_index(cx);
    }

    fn first_selectable_index(&self, cx: &App) -> usize {
        self.entries
            .iter()
            .position(|entry| {
                !matches!(entry.item, AddMenuItem::Back) && self.is_enabled(&entry.item, cx)
            })
            .unwrap_or(0)
    }

    fn server_status(&self, server_id: &ContextServerId, cx: &App) -> ContextServerStatus {
        self.context_server_store
            .as_ref()
            .and_then(|store| store.read(cx).status_for_server(server_id))
            .unwrap_or(ContextServerStatus::Stopped)
    }

    fn is_enabled(&self, item: &AddMenuItem, cx: &App) -> bool {
        match item {
            AddMenuItem::Context(ContextItem::Image) => self.supports_images,
            AddMenuItem::Context(ContextItem::Selection) => self.has_selection,
            AddMenuItem::Context(ContextItem::BranchDiff) => self.supports_embedded_context,
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

    fn navigate(&mut self, view: AddMenuView, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.view = view;
        cx.defer_in(window, |picker, window, cx| {
            picker.set_query("", window, cx);
            picker.refresh(window, cx);
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
                    Switch::new(
                        SharedString::from(format!("composer-mcp-toggle-{}", server_id.0)),
                        if is_enabled {
                            ToggleState::Selected
                        } else {
                            ToggleState::Unselected
                        },
                    )
                    .into_any_element(),
                )
            }
            _ => None,
        }
    }

    fn item_icon(&self, item: &AddMenuItem, cx: &App) -> (IconName, Color) {
        match item {
            AddMenuItem::Mode(profile_id) => profile_icon(profile_id),
            AddMenuItem::Context(context_item) => (context_item.icon(), Color::Muted),
            AddMenuItem::Skill(_) | AddMenuItem::Open(AddMenuView::Skills) => {
                (IconName::Sparkle, Color::Muted)
            }
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

impl PickerDelegate for AddMenuDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "composer-add-menu"
    }

    fn match_count(&self) -> usize {
        self.entries.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn separators_after_indices(&self) -> Vec<usize> {
        self.separators.clone()
    }

    fn set_selected_index(&mut self, ix: usize, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selected_index = ix;
        cx.notify();
    }

    fn can_select(&self, ix: usize, _window: &mut Window, cx: &mut Context<Picker<Self>>) -> bool {
        self.entries
            .get(ix)
            .is_some_and(|entry| self.is_enabled(&entry.item, cx))
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Search modes, context, skills…".into()
    }

    fn render_editor(
        &self,
        editor: &Arc<dyn ErasedEditor>,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Div> {
        Some(
            h_flex()
                .h_9()
                .px_3()
                .flex_none()
                .overflow_hidden()
                .child(div().flex_1().child(editor.render(window, cx))),
        )
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let query = query.trim().to_string();
        if query.is_empty() {
            self.show_browse_entries(cx);
            return Task::ready(());
        }

        let searchable = self.searchable_entries(cx);
        let candidates = searchable
            .iter()
            .enumerate()
            .map(|(index, entry)| StringMatchCandidate::new(index, &entry.label))
            .collect::<Vec<_>>();
        let executor = cx.background_executor().clone();

        cx.spawn_in(window, async move |picker, cx| {
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

            picker
                .update(cx, |picker, cx| {
                    let delegate = &mut picker.delegate;
                    delegate.entries = matches
                        .into_iter()
                        .filter_map(|string_match| {
                            let entry = searchable.get(string_match.candidate_id)?;
                            Some(AddMenuEntry {
                                positions: string_match.positions,
                                ..entry.clone()
                            })
                        })
                        .collect();
                    delegate.separators.clear();
                    delegate.selected_index = delegate.first_selectable_index(cx);
                    cx.notify();
                })
                .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(entry) = self.entries.get(self.selected_index).cloned() else {
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
                cx.emit(DismissEvent);
            }
            AddMenuItem::Context(context_item) => {
                let message_editor = self.message_editor.clone();
                cx.emit(DismissEvent);
                window.defer(cx, move |window, cx| {
                    context_item.apply(&message_editor, window, cx);
                });
            }
            AddMenuItem::Skill(skill) => {
                let message_editor = self.message_editor.clone();
                cx.emit(DismissEvent);
                window.defer(cx, move |window, cx| {
                    message_editor.focus_handle(cx).focus(window, cx);
                    message_editor.update(cx, |editor, cx| {
                        editor.insert_skill_crease(&skill, window, cx);
                    });
                });
            }
            AddMenuItem::Model => {
                let message_editor = self.message_editor.clone();
                cx.emit(DismissEvent);
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
                cx.emit(DismissEvent);
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

    fn select_child(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<String> {
        let AddMenuItem::Open(view) = self.entries.get(self.selected_index)?.item else {
            return None;
        };
        self.navigate(view, window, cx);
        Some(String::new())
    }

    fn select_parent(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<String> {
        if self.view == AddMenuView::Root {
            return None;
        }
        self.navigate(AddMenuView::Root, window, cx);
        Some(String::new())
    }

    fn dismissed(&mut self, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let entry = self.entries.get(ix)?;
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
            div()
                .debug_selector(move || debug_selector)
                .child(
                    ListItem::new(("composer-add-menu-item", ix))
                        .inset(true)
                        .spacing(ListItemSpacing::Sparse)
                        .toggle_state(selected)
                        .disabled(!enabled)
                        .start_slot(Icon::new(icon).size(icon_size).color(if enabled {
                            icon_color
                        } else {
                            Color::Disabled
                        }))
                        .child(
                            h_flex()
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
                        .when_some(self.render_end_slot(&entry.item, cx), |this, end_slot| {
                            this.end_slot(div().pr_2().child(end_slot))
                        }),
                )
                .into_any_element(),
        )
    }
}
