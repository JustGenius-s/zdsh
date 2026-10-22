use super::*;
use editor::{Inlay, MultiBufferOffset, SelectionEffects};
use project::InlayId;
use terminal::completions::history_suggestion;

const HISTORY_INLAY: usize = 0;

fn common_completion_prefix(completions: &[CommandCompletion]) -> &str {
    let Some(first) = completions.first() else {
        return "";
    };
    let mut prefix = first.replacement.as_str();
    for completion in &completions[1..] {
        if completion.range != first.range {
            return "";
        }
        let length = prefix
            .chars()
            .zip(completion.replacement.chars())
            .take_while(|(left, right)| left == right)
            .map(|(character, _)| character.len_utf8())
            .sum();
        prefix = &prefix[..length];
    }
    // A partial escape must not turn the next typed character into shell syntax.
    if prefix
        .chars()
        .rev()
        .take_while(|character| *character == '\\')
        .count()
        % 2
        == 1
    {
        prefix = &prefix[..prefix.len() - 1];
    }
    prefix
}

fn history_hint_word(suffix: &str) -> &str {
    let mut seen_word = false;
    let end = suffix
        .char_indices()
        .find_map(|(offset, character)| {
            if character.is_whitespace() && seen_word {
                Some(offset)
            } else {
                seen_word |= !character.is_whitespace();
                None
            }
        })
        .unwrap_or(suffix.len());
    &suffix[..end]
}

impl CommandBlockUi {
    fn input_snapshot(&self, cx: &mut App) -> Option<(String, usize)> {
        self.input.update(cx, |editor, cx| {
            let snapshot = editor.display_snapshot(cx);
            let selection = editor.selections.newest::<MultiBufferOffset>(&snapshot);
            (editor.selections.count() == 1 && selection.is_empty())
                .then(|| (editor.text(cx), selection.head().0))
        })
    }

    fn replace_input(
        &mut self,
        range: std::ops::Range<usize>,
        replacement: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.input.update(cx, |editor, cx| {
            let offset = MultiBufferOffset(range.start + replacement.len());
            editor.transact(window, cx, |editor, window, cx| {
                editor.edit(
                    [(
                        MultiBufferOffset(range.start)..MultiBufferOffset(range.end),
                        replacement,
                    )],
                    cx,
                );
                editor.change_selections(SelectionEffects::no_scroll(), window, cx, |selections| {
                    selections.select_ranges([offset..offset]);
                });
            });
            editor.focus_handle(cx).focus(window, cx);
        });
    }

    fn set_history_hint(&mut self, hint: Option<String>, cx: &mut App) {
        let text = self.input.read(cx).text(cx);
        let hint = hint.filter(|hint| hint.len() > text.len() && hint.starts_with(&text));
        self.history_hint = hint.clone();
        self.input.update(cx, |editor, cx| {
            let text = editor.text(cx);
            let inlay = hint.and_then(|hint| {
                let suffix = hint.strip_prefix(&text)?;
                if suffix.is_empty() {
                    return None;
                }
                let snapshot = editor.buffer().read(cx).snapshot(cx);
                Some(Inlay::edit_prediction(
                    HISTORY_INLAY,
                    snapshot.anchor_after(MultiBufferOffset(text.len())),
                    suffix.to_owned(),
                ))
            });
            editor.splice_inlays(
                &[InlayId::EditPrediction(HISTORY_INLAY)],
                inlay.into_iter().collect(),
                cx,
            );
        });
    }
}

impl TerminalView {
    pub(super) fn refresh_command_history(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        if self.terminal.read(cx).command_input_state() != CommandInputState::Ready {
            ui.history_task = None;
            ui.history_generation = None;
            return;
        }
        let Some(request) = self.terminal.read(cx).command_history_request() else {
            return;
        };
        let generation = request.generation;
        if ui.history_generation == Some(generation) {
            return;
        }
        ui.history_generation = Some(generation);
        let executor = cx.background_executor().clone();
        ui.history_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = executor.spawn(async move { request.load() }).await;
            this.update_in(cx, |this, window, cx| {
                if this.terminal.read(cx).command_input_state() != CommandInputState::Ready
                    || this
                        .terminal
                        .read(cx)
                        .command_history_request()
                        .map(|request| request.generation)
                        != Some(generation)
                {
                    return;
                }
                let limit = this.terminal.read(cx).command_history_line_limit();
                let Some(ui) = &mut this.command_ui else {
                    return;
                };
                ui.history_task = None;
                match result {
                    Ok(mut history) => {
                        let selected = ui
                            .history_position
                            .and_then(|position| ui.history.get(position))
                            .cloned();
                        history.extend(ui.session_history.iter().cloned());
                        let mut seen = std::collections::HashSet::new();
                        let mut history: Vec<_> = history
                            .into_iter()
                            .rev()
                            .filter(|command| seen.insert(command.clone()))
                            .take(limit)
                            .collect();
                        history.reverse();
                        ui.history = history;
                        ui.history_position = selected.and_then(|selected| {
                            ui.history.iter().rposition(|command| *command == selected)
                        });
                        ui.history_error = None;
                        ui.completion_query = None;
                        this.refresh_command_suggestions(window, cx);
                    }
                    Err(error) => {
                        log::error!("Failed to load terminal history: {error:#}");
                        ui.history_error = Some(format!("Failed to load shell history: {error:#}"));
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
    }

    pub(super) fn dismiss_command_suggestions(&mut self, cx: &mut Context<Self>) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        ui.completion_task = None;
        ui.completion_query = None;
        ui.completion_generation += 1;
        ui.completion_open = false;
        ui.completion_history = false;
        ui.completion_error = None;
        ui.completion_selection = None;
        ui.completions.clear();
        ui.set_history_hint(None, cx);
        cx.notify();
    }

    pub(super) fn refresh_command_suggestions(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ready = self.terminal.read(cx).command_input_state() == CommandInputState::Ready;
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        if !ready
            || !ui.input.focus_handle(cx).contains_focused(window, cx)
            || ui.input.update(cx, |editor, cx| {
                gpui::EntityInputHandler::marked_text_range(editor, window, cx).is_some()
            })
        {
            self.dismiss_command_suggestions(cx);
            return;
        }
        let Some((text, cursor)) = ui.input_snapshot(cx) else {
            self.dismiss_command_suggestions(cx);
            return;
        };
        if ui.completion_query.as_ref() == Some(&(text.clone(), cursor)) {
            return;
        }
        let history_menu = ui.completion_history;
        let browsing_history = ui
            .history_position
            .and_then(|position| ui.history.get(position))
            == Some(&text);
        self.dismiss_command_suggestions(cx);
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        ui.completion_query = Some((text.clone(), cursor));
        if browsing_history {
            return;
        }
        ui.history_position = None;
        ui.draft.clear();
        let hint = (cursor == text.len())
            .then(|| history_suggestion(&text, ui.history.iter().map(String::as_str)))
            .flatten();
        ui.set_history_hint(hint, cx);
        if history_menu && !text.trim().is_empty() {
            self.open_command_history(&text, cursor, cx);
        }
    }

    fn request_command_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dismiss_command_suggestions(cx);
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        let Some((text, cursor)) = ui.input_snapshot(cx) else {
            return;
        };
        ui.history_position = None;
        ui.draft.clear();
        ui.completion_query = Some((text.clone(), cursor));
        let generation = ui.completion_generation;
        let Some(request) = self.terminal.read(cx).command_completion_request() else {
            ui.completion_error = Some("Cannot determine the shell's working directory.".into());
            return;
        };
        let query = text.clone();
        let executor = cx.background_executor().clone();
        ui.completion_task = Some(cx.spawn_in(window, async move |this, cx| {
            let results = executor
                .spawn(async move { request.query(&query, cursor).await })
                .await;
            this.update_in(cx, |this, window, cx| {
                if this.terminal.read(cx).command_input_state() != CommandInputState::Ready {
                    return;
                }
                let Some(ui) = &mut this.command_ui else {
                    return;
                };
                if ui.completion_generation != generation
                    || !ui.input.focus_handle(cx).contains_focused(window, cx)
                    || ui.input_snapshot(cx) != Some((text.clone(), cursor))
                {
                    return;
                }
                ui.completion_task = None;
                match results {
                    Ok(results) => {
                        ui.completions = results;
                        if ui.completions.len() == 1 {
                            this.accept_command_completion(0, window, cx);
                            return;
                        }
                        if ui.completions.is_empty() {
                            let hint = (cursor == text.len())
                                .then(|| {
                                    history_suggestion(&text, ui.history.iter().map(String::as_str))
                                })
                                .flatten();
                            ui.set_history_hint(hint, cx);
                        } else {
                            let prefix = common_completion_prefix(&ui.completions).to_owned();
                            if let Some(range) = ui
                                .completions
                                .first()
                                .map(|completion| completion.range.clone())
                                && cursor == range.end
                                && let Some(token) = text.get(range.clone())
                                && prefix.len() > token.len()
                                && prefix.starts_with(token)
                            {
                                let mut next = text.clone();
                                next.replace_range(range.clone(), &prefix);
                                let end = range.start + prefix.len();
                                for completion in &mut ui.completions {
                                    completion.range.end = end;
                                }
                                ui.completion_query = Some((next, end));
                                ui.replace_input(range, prefix, window, cx);
                            }
                            ui.completion_open = true;
                            ui.completion_selection = Some(0);
                            ui.completion_scroll
                                .scroll_to_item(0, gpui::ScrollStrategy::Top);
                        }
                    }
                    Err(error) => {
                        ui.completion_error = Some(format!("Completion unavailable: {error}"))
                    }
                }
                cx.notify();
            })
            .log_err();
        }));
    }

    fn accept_command_completion(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        let Some(completion) = ui.completions.get(index).cloned() else {
            return;
        };
        let Some((text, cursor)) = ui.input_snapshot(cx) else {
            return;
        };
        if ui.completion_query.as_ref() != Some(&(text.clone(), cursor))
            || text.get(completion.range.clone()).is_none()
        {
            return;
        }
        self.dismiss_command_suggestions(cx);
        if let Some(ui) = &mut self.command_ui {
            ui.replace_input(completion.range, completion.replacement, window, cx);
        }
        self.refresh_command_suggestions(window, cx);
    }

    fn open_command_history(&mut self, text: &str, cursor: usize, cx: &mut Context<Self>) -> bool {
        let Some(ui) = &mut self.command_ui else {
            return false;
        };
        let mut matches = Vec::<CommandCompletion>::new();
        for command in ui.history.iter().rev() {
            if !command.is_empty()
                && command.starts_with(text)
                && !matches.iter().any(|item| item.replacement == *command)
            {
                matches.push(CommandCompletion {
                    label: command.clone(),
                    replacement: command.clone(),
                    description: Some("History".into()),
                    range: 0..text.len(),
                });
                if matches.len() == 12 {
                    break;
                }
            }
        }
        if matches.is_empty() {
            return false;
        }
        self.dismiss_command_suggestions(cx);
        let Some(ui) = &mut self.command_ui else {
            return false;
        };
        ui.completion_query = Some((text.to_owned(), cursor));
        ui.completion_open = true;
        ui.completion_history = true;
        ui.completion_selection = Some(0);
        ui.completion_scroll
            .scroll_to_item(0, gpui::ScrollStrategy::Top);
        let hint = (cursor == text.len())
            .then(|| matches.first().map(|item| item.replacement.clone()))
            .flatten();
        ui.completions = matches;
        ui.set_history_hint(hint, cx);
        true
    }

    fn navigate_command_history(
        &mut self,
        previous: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ui) = &mut self.command_ui else {
            return false;
        };
        if ui.history.is_empty() {
            return false;
        }
        if !previous && ui.history_position.is_none() {
            return true;
        }
        let position = ui.history_position.unwrap_or(ui.history.len());
        if ui.history_position.is_none() {
            ui.draft = ui.input.read(cx).text(cx);
        }
        let position = if previous {
            position.saturating_sub(1)
        } else {
            (position + 1).min(ui.history.len())
        };
        ui.history_position = (position < ui.history.len()).then_some(position);
        let text = ui.history.get(position).unwrap_or(&ui.draft).clone();
        self.dismiss_command_suggestions(cx);
        if let Some(ui) = &mut self.command_ui {
            let length = ui.input.read(cx).text(cx).len();
            ui.completion_query = Some((text.clone(), text.len()));
            ui.replace_input(0..length, text, window, cx);
        }
        true
    }

    pub(super) fn command_suggestion_key(
        &mut self,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(ui) = &mut self.command_ui else {
            return false;
        };
        if key.modifiers.control
            && !key.modifiers.alt
            && !key.modifiers.platform
            && !key.modifiers.shift
        {
            match key.key.as_str() {
                "space" => {
                    self.request_command_completion(window, cx);
                    return true;
                }
                "p" | "n" => return self.navigate_command_history(key.key == "p", window, cx),
                _ => return false,
            }
        }
        if key.key == "right"
            && !key.modifiers.control
            && !key.modifiers.platform
            && !key.modifiers.shift
            && let Some(hint) = ui.history_hint.clone()
            && let Some((text, cursor)) = ui.input_snapshot(cx)
            && cursor == text.len()
            && hint.starts_with(&text)
            && hint.len() > text.len()
        {
            let suffix = &hint[text.len()..];
            let suffix = if key.modifiers.alt {
                history_hint_word(suffix)
            } else {
                suffix
            };
            let remaining_hint = (text.len() + suffix.len() < hint.len()).then(|| hint.clone());
            let replacement = suffix.to_owned();
            self.dismiss_command_suggestions(cx);
            if let Some(ui) = &mut self.command_ui {
                ui.replace_input(cursor..cursor, replacement, window, cx);
            }
            self.refresh_command_suggestions(window, cx);
            if let Some(ui) = &mut self.command_ui {
                ui.set_history_hint(remaining_hint, cx);
            }
            return true;
        }
        if key.modifiers.control || key.modifiers.alt || key.modifiers.platform {
            return false;
        }
        if key.key == "escape"
            && (ui.completion_open || ui.history_hint.is_some() || ui.completion_task.is_some())
        {
            self.dismiss_command_suggestions(cx);
            return true;
        }
        if ui.completion_open {
            match key.key.as_str() {
                "up" | "down" | "tab" if !key.modifiers.shift || key.key == "tab" => {
                    if key.key == "tab" && !key.modifiers.shift {
                        let selected = ui.completion_selection.unwrap_or(0);
                        self.accept_command_completion(selected, window, cx);
                    } else {
                        let count = ui.completions.len();
                        if count == 0 {
                            return true;
                        }
                        let index = ui.completion_selection.unwrap_or(0);
                        let selected = if key.key == "up" || key.modifiers.shift {
                            (index + count - 1) % count
                        } else {
                            (index + 1) % count
                        };
                        ui.completion_selection = Some(selected);
                        ui.completion_scroll
                            .scroll_to_item(selected, gpui::ScrollStrategy::Nearest);
                        if ui.completion_history {
                            let at_end = ui
                                .input_snapshot(cx)
                                .is_some_and(|(text, cursor)| cursor == text.len());
                            let hint = at_end
                                .then(|| {
                                    ui.completions
                                        .get(selected)
                                        .map(|item| item.replacement.clone())
                                })
                                .flatten();
                            ui.set_history_hint(hint, cx);
                        }
                    }
                    return true;
                }
                "enter" if !key.modifiers.shift => {
                    let selected = ui.completion_selection.unwrap_or(0);
                    self.accept_command_completion(selected, window, cx);
                    return true;
                }
                _ => {}
            }
        }
        if key.key == "tab" && !key.modifiers.shift {
            self.request_command_completion(window, cx);
            return true;
        }
        if (key.key == "up" || key.key == "down")
            && !key.modifiers.shift
            && let Some((text, cursor)) = ui.input_snapshot(cx)
        {
            let previous = key.key == "up";
            let allowed = if previous {
                !text[..cursor].contains('\n')
            } else {
                !text[cursor..].contains('\n')
            };
            if allowed {
                if previous
                    && ui.history_position.is_none()
                    && !text.trim().is_empty()
                    && self.open_command_history(&text, cursor, cx)
                {
                    return true;
                }
                return self.navigate_command_history(previous, window, cx);
            }
        }
        false
    }

    pub(super) fn render_command_suggestions(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(ui) = &self.command_ui else {
            return div().into_any_element();
        };
        if !ui.completion_open {
            return div().into_any_element();
        }
        let Some(bounds) = ui.input.read(cx).last_bounds().copied() else {
            return div().into_any_element();
        };
        let menu_height = px(28. * ui.completions.len().min(6) as f32 + 8.);
        let space_below = window.viewport_size().height - bounds.bottom() - px(8.);
        let open_below = space_below >= menu_height || space_below >= bounds.top();
        let (position, anchor) = if open_below {
            (
                bounds.bottom_left() + gpui::point(px(0.), px(4.)),
                gpui::Anchor::TopLeft,
            )
        } else {
            (
                bounds.origin - gpui::point(px(0.), px(4.)),
                gpui::Anchor::BottomLeft,
            )
        };
        gpui::deferred(
            gpui::anchored()
                .position(position)
                .anchor(anchor)
                .snap_to_window_with_margin(px(8.))
                .child(
                    v_flex()
                        .id("terminal-completion-menu")
                        .occlude()
                        .w(bounds.size.width.min(px(440.)))
                        .p_1()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .bg(cx.theme().colors().elevated_surface_background)
                        .rounded_md()
                        .shadow_lg()
                        .on_mouse_down_out(
                            cx.listener(|this, _, _, cx| this.dismiss_command_suggestions(cx)),
                        )
                        .child(
                            gpui::uniform_list(
                                "command-completions",
                                ui.completions.len(),
                                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                                    let Some(ui) = &this.command_ui else {
                                        return Vec::new();
                                    };
                                    range
                                        .filter_map(|index| {
                                            ui.completions.get(index).map(|completion| {
                                                h_flex()
                                                    .id(("command-completion", index))
                                                    .h(px(28.))
                                                    .w_full()
                                                    .gap_2()
                                                    .px_2()
                                                    .rounded_sm()
                                                    .cursor_pointer()
                                                    .when(
                                                        ui.completion_selection == Some(index),
                                                        |this| {
                                                            this.bg(cx
                                                                .theme()
                                                                .colors()
                                                                .element_selected)
                                                        },
                                                    )
                                                    .hover(|this| {
                                                        this.bg(cx.theme().colors().element_hover)
                                                    })
                                                    .on_mouse_down(
                                                        MouseButton::Left,
                                                        cx.listener(move |this, _, window, cx| {
                                                            window.prevent_default();
                                                            cx.stop_propagation();
                                                            this.accept_command_completion(
                                                                index, window, cx,
                                                            );
                                                        }),
                                                    )
                                                    .child(
                                                        div().flex_1().min_w_0().child(
                                                            Label::new(completion.label.clone())
                                                                .size(LabelSize::Small)
                                                                .truncate(),
                                                        ),
                                                    )
                                                    .when_some(
                                                        completion.description.clone(),
                                                        |this, description| {
                                                            this.child(
                                                                div()
                                                                    .max_w(px(160.))
                                                                    .min_w_0()
                                                                    .child(
                                                                        Label::new(description)
                                                                            .size(LabelSize::XSmall)
                                                                            .color(Color::Muted)
                                                                            .truncate(),
                                                                    ),
                                                            )
                                                        },
                                                    )
                                            })
                                        })
                                        .collect()
                                }),
                            )
                            .w_full()
                            .h(px(28. * ui.completions.len().min(6) as f32))
                            .track_scroll(&ui.completion_scroll),
                        ),
                ),
        )
        .with_priority(1)
        .into_any_element()
    }
}
