use super::composer::{PillButton, composer_surface};
use super::tool_calls::diff_stat;
use super::transcript::kit_icon;
use super::*;
use gpui_kit::component::{
    Icon as KitIcon, Size as KitSize, Theme as KitTheme, button::ButtonVariant,
    shimmer::ShimmerText, spinner::Spinner,
};

impl ThreadView {
    /// The card over the composer with what the thread is working through: requests awaiting
    /// permission, the plan, the files edited so far, and queued messages.
    pub fn render_activity_bar(
        &self,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        let thread = self.thread.read(cx);
        let action_log = thread.action_log();
        let telemetry = ActionLogTelemetry::from(thread);
        let plan = thread.plan().filter(|plan| !plan.is_empty());

        let awaiting_permission = self
            .render_main_agent_awaiting_permission(window, cx)
            .or_else(|| self.render_subagents_awaiting_permission(cx));
        let generic_permissions = self.render_generic_permissions(cx);

        // Collapsed edits live in the composer's Changes chip; the activity bar
        // only carries the expanded file list.
        let changed_buffers = if self.edits_expanded && thread.parent_session_id().is_none() {
            action_log.read(cx).changed_buffers(cx).collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Temporarily always enable ACP edit controls. This is temporary, to lessen the
        // impact of a nasty bug that causes them to sometimes be disabled when they shouldn't
        // be, which blocks you from being able to accept or reject edits. This switches the
        // bug to be that sometimes it's enabled when it shouldn't be, which at least doesn't
        // block you from using the panel.
        let pending_edits = false;

        let mut sections = Vec::new();
        sections.extend(generic_permissions);
        sections.extend(awaiting_permission);
        if let Some(plan) = plan {
            sections.push(
                v_flex()
                    .child(self.render_plan_summary(plan, window, cx))
                    .when(self.plan_expanded, |this| {
                        this.child(self.render_plan_entries(plan, window, cx))
                    })
                    .into_any_element(),
            );
        }
        if !changed_buffers.is_empty() {
            sections.push(
                v_flex()
                    .child(self.render_edits_summary(&changed_buffers, pending_edits, cx))
                    .child(self.render_edited_files(
                        action_log,
                        telemetry,
                        &changed_buffers,
                        pending_edits,
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        if self.has_queued_messages() {
            sections.push(
                v_flex()
                    .child(self.render_message_queue_summary(cx))
                    .when(self.queue_expanded, |this| {
                        this.child(self.render_message_queue_entries(window, cx))
                    })
                    .into_any_element(),
            );
        }

        if sections.is_empty() {
            return None;
        }

        let (background, border) = composer_surface(cx);
        let card = v_flex()
            .w_full()
            .rounded(px(16.))
            .border_1()
            .border_color(border)
            .bg(background)
            .overflow_hidden()
            .children(sections.into_iter().enumerate().map(|(index, section)| {
                div()
                    .when(index > 0, |this| this.border_t_1().border_color(border))
                    .child(section)
            }));

        Some(
            composer_column(card, AgentSettings::get_global(cx).max_content_width)
                .into_any_element(),
        )
    }

    fn render_plan_summary(
        &self,
        plan: &Plan,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let plan_expanded = self.plan_expanded;
        let stats = plan.stats();
        let theme = KitTheme::global(cx);
        let (background, _) = composer_surface(cx);

        let title = if let Some(entry) = stats.in_progress_entry
            && !plan_expanded
        {
            h_flex()
                .relative()
                .flex_1()
                .min_w_0()
                .gap_1()
                .overflow_hidden()
                .child(div().flex_none().child("Current:"))
                .child(div().text_xs().line_clamp(1).child(MarkdownElement::new(
                    entry.content.clone(),
                    plan_label_markdown_style(&entry.source.status, window, cx),
                )))
                .when(stats.pending > 0, |this| {
                    this.child(
                        h_flex()
                            .absolute()
                            .top_0()
                            .right_0()
                            .h_full()
                            .child(div().min_w_8().h_full().bg(linear_gradient(
                                90.,
                                linear_color_stop(background, 1.),
                                linear_color_stop(background.opacity(0.2), 0.),
                            )))
                            .child(
                                div()
                                    .h_full()
                                    .flex()
                                    .items_center()
                                    .pr_0p5()
                                    .bg(background)
                                    .child(format!("{} left", stats.pending)),
                            ),
                    )
                })
        } else {
            let status_label = plan_summary_label(&stats, plan.entries.len());

            h_flex()
                .debug_selector({
                    let status_label = status_label.clone();
                    move || format!("plan-status-{status_label}")
                })
                .flex_1()
                .min_w_0()
                .gap_1()
                .justify_between()
                .child("Plan")
                .child(
                    div()
                        .pr_1()
                        .text_color(theme.foreground)
                        .child(status_label),
                )
        };

        section_header("plan_summary", plan_expanded, cx)
            .debug_selector(|| "plan_summary".into())
            .child(title)
            .child(
                PillButton::new("dismiss-plan")
                    .size(KitSize::XSmall)
                    .icon(IconName::Close)
                    .debug_selector("ICON-Close")
                    .tooltip(Tooltip::text("Clear Plan"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.thread.update(cx, |thread, cx| thread.clear_plan(cx));
                    })),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.plan_expanded = !this.plan_expanded;
                cx.notify();
            }))
    }

    fn render_plan_entries(
        &self,
        plan: &Plan,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = KitTheme::global(cx);
        let (background, _) = composer_surface(cx);

        v_flex()
            .id("plan_items_list")
            .max_h_40()
            .pb_1()
            .overflow_y_scroll()
            .children(plan.entries.iter().enumerate().map(|(index, entry)| {
                let tooltip_text: SharedString = entry.content.read(cx).source().to_string().into();
                let (icon_name, icon_color) = match &entry.source.status {
                    acp_v2::PlanEntryStatus::InProgress => (IconName::TodoProgress, theme.info),
                    acp_v2::PlanEntryStatus::Completed => (IconName::TodoComplete, theme.success),
                    acp_v2::PlanEntryStatus::Cancelled => (IconName::Close, theme.muted_foreground),
                    _ => (IconName::TodoPending, theme.muted_foreground),
                };
                let icon = if matches!(entry.source.status, acp_v2::PlanEntryStatus::InProgress) {
                    Spinner::new()
                        .icon(kit_icon(icon_name))
                        .color(icon_color)
                        .with_size(KitSize::Small)
                        .into_any_element()
                } else {
                    kit_icon(icon_name)
                        .with_size(KitSize::Small)
                        .text_color(icon_color)
                        .into_any_element()
                };

                h_flex()
                    .id(("plan_entry_row", index))
                    .debug_selector(|| format!("plan-entry-{index}-{icon_name:?}"))
                    .relative()
                    .h_7()
                    .pl_3()
                    .pr_2()
                    .gap_1p5()
                    .overflow_hidden()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(div().flex_none().child(icon))
                    .child(div().min_w_0().child(MarkdownElement::new(
                        entry.content.clone(),
                        plan_label_markdown_style(&entry.source.status, window, cx),
                    )))
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .h_full()
                            .w_8()
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(background, 1.),
                                linear_color_stop(background.opacity(0.), 0.),
                            )),
                    )
                    .tooltip(Tooltip::text(tooltip_text))
            }))
    }

    fn render_edits_summary(
        &self,
        changed_buffers: &[(Entity<Buffer>, Entity<BufferDiff>)],
        pending_edits: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        const EDIT_NOT_READY_TOOLTIP_LABEL: &str = "Wait until file edits are complete.";

        let focus_handle = self.focus_handle(cx);
        let theme = KitTheme::global(cx);
        let file_count = changed_buffers.len();
        let files_label = format!("{file_count} {}", pluralize("file", file_count));
        let keybinding_size = rems_from_px(12_f32);

        section_header("edits-container", true, cx)
            .map(|this| {
                if pending_edits {
                    this.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .child(ShimmerText::new(format!("Editing {files_label}…"))),
                    )
                } else {
                    let stats = DiffStats::all_files(changed_buffers.iter().cloned(), cx);
                    this.child(div().flex_none().child("Edits"))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.foreground)
                                .child(files_label),
                        )
                        .child(div().flex_none().child(diff_stat(stats, cx)))
                }
            })
            .child(div().flex_1())
            .child(
                h_flex()
                    .flex_none()
                    .gap_0p5()
                    .child(
                        PillButton::new("review-changes")
                            .size(KitSize::XSmall)
                            .icon(IconName::ListTodo)
                            .tooltip({
                                let focus_handle = focus_handle.clone();
                                move |_window, cx| {
                                    Tooltip::for_action_in(
                                        "Review Changes",
                                        &OpenAgentDiff,
                                        &focus_handle,
                                        cx,
                                    )
                                }
                            })
                            .on_click(cx.listener(|_, _, window, cx| {
                                window.dispatch_action(OpenAgentDiff.boxed_clone(), cx);
                            })),
                    )
                    .child(
                        PillButton::new("reject-all-changes")
                            .size(KitSize::XSmall)
                            .label("Reject All")
                            .disabled(pending_edits)
                            .when(pending_edits, |this| {
                                this.tooltip(Tooltip::text(EDIT_NOT_READY_TOOLTIP_LABEL))
                            })
                            .child(
                                KeyBinding::for_action_in(&RejectAll, &focus_handle, cx)
                                    .size(keybinding_size),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reject_all(&RejectAll, window, cx);
                            })),
                    )
                    .child(
                        PillButton::new("keep-all-changes")
                            .size(KitSize::XSmall)
                            .variant(ButtonVariant::Secondary)
                            .label("Keep All")
                            .disabled(pending_edits)
                            .when(pending_edits, |this| {
                                this.tooltip(Tooltip::text(EDIT_NOT_READY_TOOLTIP_LABEL))
                            })
                            .child(
                                KeyBinding::for_action_in(&KeepAll, &focus_handle, cx)
                                    .size(keybinding_size),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.keep_all(&KeepAll, window, cx);
                            })),
                    ),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.edits_expanded = !this.edits_expanded;
                cx.notify();
            }))
    }

    fn render_edited_files(
        &self,
        action_log: &Entity<ActionLog>,
        telemetry: ActionLogTelemetry,
        changed_buffers: &[(Entity<Buffer>, Entity<BufferDiff>)],
        pending_edits: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let theme = KitTheme::global(cx);
        let (background, _) = composer_surface(cx);
        let hovered_background = background.blend(theme.list_hover);

        // Sort edited files alphabetically for consistency with Git diff view
        let mut sorted_buffers: Vec<_> = changed_buffers.iter().collect();
        sorted_buffers.sort_by(|(buffer_a, _), (buffer_b, _)| {
            let path_a = buffer_a.read(cx).file().map(|f| f.path().clone());
            let path_b = buffer_b.read(cx).file().map(|f| f.path().clone());
            path_a.cmp(&path_b)
        });

        v_flex()
            .id("edited_files_list")
            .max_h_40()
            .pb_1()
            .overflow_y_scroll()
            .children(
                sorted_buffers
                    .into_iter()
                    .enumerate()
                    .flat_map(|(index, (buffer, diff))| {
                        let file = buffer.read(cx).file()?;
                        let path = file.path();
                        let path_style = file.path_style(cx);
                        let separator = path_style.primary_separator();
                        let fallback_full_path =
                            full_path_for_empty_project_path(file.as_ref(), cx);

                        let parent_path = path
                            .parent()
                            .filter(|parent| !parent.is_empty())
                            .map(|parent| format!("{}{separator}", parent.display(path_style)));
                        let file_name = path
                            .file_name()
                            .map(|name| name.to_string())
                            .or_else(|| fallback_full_path.clone())?;
                        let full_path = fallback_full_path
                            .unwrap_or_else(|| path.display(path_style).to_string());
                        let icon_path = FileIcons::get_icon(path.as_std_path(), cx)
                            .unwrap_or_else(|| IconName::File.path().into());
                        let file_stats = DiffStats::single_file(diff.read(cx));

                        let buttons = self.render_edited_file_buttons(
                            index,
                            buffer,
                            action_log,
                            &telemetry,
                            pending_edits,
                            cx,
                        );

                        Some(
                            h_flex()
                                .id(("file-container", index))
                                .group("edited-code")
                                .relative()
                                .h_7()
                                .min_w_0()
                                .pl_3()
                                .pr_2()
                                .hover(|style| style.bg(hovered_background))
                                .child(
                                    h_flex()
                                        .id(("file-name-path", index))
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1p5()
                                        .cursor_pointer()
                                        .text_xs()
                                        .child(
                                            KitIcon::empty()
                                                .path(icon_path)
                                                .with_size(KitSize::Small)
                                                .text_color(theme.muted_foreground)
                                                .flex_none(),
                                        )
                                        .child(
                                            div()
                                                .flex_none()
                                                .text_color(theme.foreground)
                                                .child(file_name),
                                        )
                                        .children(parent_path.map(|parent_path| {
                                            div()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme.muted_foreground)
                                                .child(parent_path)
                                        }))
                                        .child(div().flex_none().child(diff_stat(file_stats, cx)))
                                        .tooltip(move |_, cx| {
                                            Tooltip::with_meta(
                                                "Go to File",
                                                None,
                                                full_path.clone(),
                                                cx,
                                            )
                                        })
                                        .on_click({
                                            let buffer = buffer.clone();
                                            cx.listener(move |this, _, window, cx| {
                                                this.open_edited_buffer(&buffer, window, cx);
                                            })
                                        }),
                                )
                                .child(
                                    h_flex()
                                        .visible_on_hover("edited-code")
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .h_full()
                                        .pl_2()
                                        .pr_2()
                                        .bg(hovered_background)
                                        .child(buttons),
                                ),
                        )
                    }),
            )
    }

    fn render_edited_file_buttons(
        &self,
        index: usize,
        buffer: &Entity<Buffer>,
        action_log: &Entity<ActionLog>,
        telemetry: &ActionLogTelemetry,
        pending_edits: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .gap_0p5()
            .child(
                PillButton::new(("review", index))
                    .size(KitSize::XSmall)
                    .label("Review")
                    .on_click({
                        let buffer = buffer.clone();
                        cx.listener(move |this, _, window, cx| {
                            this.open_edited_buffer(&buffer, window, cx);
                        })
                    }),
            )
            .child(
                PillButton::new(("reject-file", index))
                    .size(KitSize::XSmall)
                    .label("Reject")
                    .disabled(pending_edits)
                    .on_click({
                        let buffer = buffer.clone();
                        let action_log = action_log.clone();
                        let telemetry = telemetry.clone();
                        move |_, _, cx| {
                            action_log.update(cx, |action_log, cx| {
                                action_log
                                    .reject_edits_in_ranges(
                                        buffer.clone(),
                                        vec![Anchor::min_max_range_for_buffer(
                                            buffer.read(cx).remote_id(),
                                        )],
                                        Some(telemetry.clone()),
                                        cx,
                                    )
                                    .0
                                    .detach_and_log_err(cx);
                            })
                        }
                    }),
            )
            .child(
                PillButton::new(("keep-file", index))
                    .size(KitSize::XSmall)
                    .variant(ButtonVariant::Secondary)
                    .label("Keep")
                    .disabled(pending_edits)
                    .on_click({
                        let buffer = buffer.clone();
                        let action_log = action_log.clone();
                        let telemetry = telemetry.clone();
                        move |_, _, cx| {
                            action_log.update(cx, |action_log, cx| {
                                action_log.keep_edits_in_range(
                                    buffer.clone(),
                                    Anchor::min_max_range_for_buffer(buffer.read(cx).remote_id()),
                                    Some(telemetry.clone()),
                                    cx,
                                );
                            })
                        }
                    }),
            )
    }

    fn render_message_queue_summary(&self, cx: &Context<Self>) -> impl IntoElement {
        let queue_count = self.message_queue.len();
        let title: SharedString = if queue_count == 1 {
            "1 Queued Message".into()
        } else {
            format!("{} Queued Messages", queue_count).into()
        };

        section_header("queue_summary", self.queue_expanded, cx)
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(
                PillButton::new("clear_queue")
                    .size(KitSize::XSmall)
                    .label("Clear All")
                    .child(
                        KeyBinding::for_action(&ClearMessageQueue, cx).size(rems_from_px(12_f32)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_queue(cx);
                    })),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.queue_expanded = !this.queue_expanded;
                cx.notify();
            }))
    }

    fn render_queue_steer_button(
        &self,
        entry_id: QueueEntryId,
        index: usize,
        is_next: bool,
        steer_on: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let focus_handle = self.message_editor.focus_handle(cx);

        PillButton::new(("steer", index))
            .size(KitSize::XSmall)
            .label("Steer")
            .toggle_state(steer_on)
            .when(is_next, |this| {
                this.child(
                    KeyBinding::for_action_in(&ToggleSteerFirstQueuedMessage, &focus_handle, cx)
                        .size(rems_from_px(12_f32)),
                )
            })
            .tooltip(move |_window, cx| {
                Tooltip::with_meta(
                    "Steer",
                    None,
                    "Interrupt the agent at its next step to send this message. \
                     When off, queued messages wait for the agent to finish.",
                    cx,
                )
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_queue_entry_steer(entry_id, cx);
            }))
    }

    fn render_message_queue_entries(
        &self,
        window: &mut Window,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let message_editor = self.message_editor.read(cx);
        let focus_handle = message_editor.focus_handle(cx);
        let theme = KitTheme::global(cx);

        let can_fast_track = self.message_queue.can_fast_track();
        let is_native = self.as_native_thread(cx).is_some();
        let keybinding_size = rems_from_px(12_f32);

        v_flex()
            .id("message_queue_list")
            .max_h_40()
            .pb_1()
            .overflow_y_scroll()
            .children(self.message_queue.iter().enumerate().map(|(index, entry)| {
                let entry_id = entry.id;
                let editor = &entry.editor;
                let is_next = index == 0;
                let (indicator_color, tooltip_text) = if is_next {
                    (theme.info, "Next in Queue")
                } else {
                    (theme.muted_foreground, "In Queue")
                };

                let editor_focused = editor.focus_handle(cx).is_focused(window);
                let steer_on = entry.steer;

                h_flex()
                    .group("queue_entry")
                    .w_full()
                    .flex_wrap()
                    .pl_3()
                    .pr_2()
                    .py_1()
                    .gap_1p5()
                    .child(
                        div()
                            .id("next_in_queue")
                            .flex_none()
                            .child(
                                kit_icon(IconName::Circle)
                                    .with_size(KitSize::XSmall)
                                    .text_color(indicator_color),
                            )
                            .tooltip(Tooltip::text(tooltip_text)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(rems_from_px(120_f32))
                            .child(editor.clone()),
                    )
                    .child(if editor_focused {
                        h_flex()
                            .gap_0p5()
                            .ml_auto()
                            .justify_end()
                            .child(
                                PillButton::new(("edit", index))
                                    .size(KitSize::XSmall)
                                    .icon(IconName::Pencil)
                                    .tooltip(|_window, cx| {
                                        Tooltip::with_meta(
                                            "Edit Queued Message",
                                            None,
                                            "Type anything to edit",
                                            cx,
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.move_queued_message_to_main_editor(
                                            entry_id, None, None, window, cx,
                                        );
                                    })),
                            )
                            .when(is_native, |row| {
                                row.child(self.render_queue_steer_button(
                                    entry_id, index, is_next, steer_on, cx,
                                ))
                            })
                            .child(
                                PillButton::new(("send_now_focused", index))
                                    .size(KitSize::XSmall)
                                    .outline()
                                    .label("Send Now")
                                    .child(
                                        KeyBinding::for_action_in(
                                            &SendImmediately,
                                            &editor.focus_handle(cx),
                                            cx,
                                        )
                                        .size(keybinding_size),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.send_queued_message_now(entry_id, window, cx);
                                    })),
                            )
                    } else {
                        h_flex()
                            .when(!is_next, |this| this.visible_on_hover("queue_entry"))
                            .gap_0p5()
                            .ml_auto()
                            .justify_end()
                            .child(
                                PillButton::new(("delete", index))
                                    .size(KitSize::XSmall)
                                    .icon(IconName::Trash)
                                    .tooltip({
                                        let focus_handle = focus_handle.clone();
                                        move |_window, cx| {
                                            if is_next {
                                                Tooltip::for_action_in(
                                                    "Remove Message from Queue",
                                                    &RemoveFirstQueuedMessage,
                                                    &focus_handle,
                                                    cx,
                                                )
                                            } else {
                                                Tooltip::simple("Remove Message from Queue", cx)
                                            }
                                        }
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_from_queue(entry_id, cx);
                                        cx.notify();
                                    })),
                            )
                            .child(
                                PillButton::new(("edit", index))
                                    .size(KitSize::XSmall)
                                    .icon(IconName::Pencil)
                                    .tooltip({
                                        let focus_handle = focus_handle.clone();
                                        move |_window, cx| {
                                            if is_next {
                                                Tooltip::for_action_in(
                                                    "Edit",
                                                    &EditFirstQueuedMessage,
                                                    &focus_handle,
                                                    cx,
                                                )
                                            } else {
                                                Tooltip::simple("Edit", cx)
                                            }
                                        }
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.move_queued_message_to_main_editor(
                                            entry_id, None, None, window, cx,
                                        );
                                    })),
                            )
                            .when(is_native, |row| {
                                row.child(self.render_queue_steer_button(
                                    entry_id, index, is_next, steer_on, cx,
                                ))
                            })
                            .child(
                                PillButton::new(("send_now", index))
                                    .size(KitSize::XSmall)
                                    .when(is_next, |this| this.outline())
                                    .label("Send Now")
                                    .when(is_next && message_editor.is_empty(cx), |this| {
                                        let action: Box<dyn gpui::Action> = if can_fast_track {
                                            Box::new(Chat)
                                        } else {
                                            Box::new(SendNextQueuedMessage)
                                        };

                                        this.child(
                                            KeyBinding::for_action_in(
                                                action.as_ref(),
                                                &focus_handle,
                                                cx,
                                            )
                                            .size(keybinding_size),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.send_queued_message_now(entry_id, window, cx);
                                    })),
                            )
                    })
            }))
    }
}

/// The row at the top of a section of the activity bar, which opens and closes it.
fn section_header(id: impl Into<ElementId>, expanded: bool, cx: &App) -> Stateful<Div> {
    let theme = KitTheme::global(cx);
    let chevron = if expanded {
        IconName::ChevronDown
    } else {
        IconName::ChevronRight
    };

    h_flex()
        .id(id)
        .h_8()
        .w_full()
        .min_w_0()
        .pl_3()
        .pr_1p5()
        .gap_1p5()
        .cursor_pointer()
        .text_size(rems_from_px(13_f32))
        .text_color(theme.muted_foreground)
        .hover(|style| style.bg(theme.list_hover))
        .child(kit_icon(chevron).with_size(KitSize::XSmall).flex_none())
}
