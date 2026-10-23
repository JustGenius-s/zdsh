mod completions;

use super::*;
use editor::EditorEvent;
use gpui::{ClipboardItem, EntityId, ScrollHandle};
use terminal::completions::CommandCompletion;
use terminal::prompt_context::PromptContext;
use terminal::{CommandBlock, CommandInputState};
use util::paths::PathExt as _;

pub(super) const COMMAND_CONTENT_INSET: Rems = gpui::rems(0.75);

pub(super) struct CommandBlockUi {
    input: Entity<Editor>,
    input_style: Option<gpui::TextStyleRefinement>,
    blocks: Vec<BlockView>,
    scroll: ScrollHandle,
    state: CommandInputState,
    history: Vec<String>,
    session_history: Vec<String>,
    history_generation: Option<u64>,
    history_task: Option<Task<()>>,
    history_error: Option<String>,
    history_position: Option<usize>,
    draft: String,
    error: Option<String>,
    prompt_context: PromptContext,
    prompt_generation: Option<u64>,
    prompt_task: Option<Task<()>>,
    completions: Vec<CommandCompletion>,
    completion_selection: Option<usize>,
    completion_scroll: gpui::UniformListScrollHandle,
    completion_query: Option<(String, usize)>,
    completion_task: Option<Task<()>>,
    completion_generation: u64,
    completion_open: bool,
    completion_history: bool,
    completion_error: Option<String>,
    history_hint: Option<String>,
    _input_subscription: Subscription,
    _key_subscription: Subscription,
}

struct BlockView {
    id: EntityId,
    view: Entity<TerminalView>,
    block: CommandBlock,
    collapsed: bool,
}

impl CommandBlockUi {
    pub fn new(window: &mut Window, cx: &mut Context<TerminalView>) -> Self {
        let input = cx.new(|cx| {
            let mut editor = Editor::auto_height(1, 6, window, cx);
            editor.set_offset_content(false, cx);
            editor.set_placeholder_text("Type a command", window, cx);
            editor.set_read_only(true);
            editor
        });
        let subscription =
            cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
                EditorEvent::Focused
                    if this.terminal.read(cx).command_input_state() != CommandInputState::Ready =>
                {
                    this.focus_handle.focus(window, cx);
                }
                EditorEvent::BufferEdited
                | EditorEvent::SelectionsChanged { .. }
                | EditorEvent::Focused => {
                    this.refresh_command_suggestions(window, cx);
                }
                EditorEvent::Blurred => this.dismiss_command_suggestions(cx),
                _ => {}
            });
        // Key bindings run before capture_key_down, so editor::Tab would indent
        // before the command input could request completions.
        let listener = cx.listener(|this, event: &gpui::KeystrokeEvent, window, cx| {
            if event.input_preference == gpui::InputPreference::CharacterInput
                || this.terminal.read(cx).command_input_state() != CommandInputState::Ready
                || !this
                    .command_ui
                    .as_ref()
                    .is_some_and(|ui| ui.input.focus_handle(cx).is_focused(window))
            {
                return;
            }
            this.command_input_key(&event.keystroke, window, cx);
        });
        let key_subscription = cx.intercept_keystrokes(listener);
        Self {
            input,
            input_style: None,
            blocks: Vec::new(),
            scroll: ScrollHandle::new(),
            state: CommandInputState::Starting,
            history: Vec::new(),
            session_history: Vec::new(),
            history_generation: None,
            history_task: None,
            history_error: None,
            history_position: None,
            draft: String::new(),
            error: None,
            prompt_context: PromptContext::default(),
            prompt_generation: None,
            prompt_task: None,
            completions: Vec::new(),
            completion_selection: None,
            completion_scroll: gpui::UniformListScrollHandle::new(),
            completion_query: None,
            completion_task: None,
            completion_generation: 0,
            completion_open: false,
            completion_history: false,
            completion_error: None,
            history_hint: None,
            _input_subscription: subscription,
            _key_subscription: key_subscription,
        }
    }
}

impl TerminalView {
    fn refresh_prompt_context(&mut self, cx: &mut Context<Self>) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        if self.terminal.read(cx).command_input_state() != CommandInputState::Ready {
            ui.prompt_task = None;
            ui.prompt_generation = None;
            return;
        }
        let Some(request) = self.terminal.read(cx).prompt_context_request() else {
            return;
        };
        if ui.prompt_generation == Some(request.generation) {
            return;
        }
        let generation = request.generation;
        ui.prompt_generation = Some(generation);
        let directory = request.display_directory();
        if ui.prompt_context.directory != directory {
            ui.prompt_context = PromptContext {
                directory,
                ..Default::default()
            };
        }
        let executor = cx.background_executor().clone();
        ui.prompt_task = Some(cx.spawn(async move |this, cx| {
            let context = executor.spawn(request.query(executor.clone())).await;
            this.update(cx, |this, cx| {
                if this.terminal.read(cx).command_input_state() != CommandInputState::Ready
                    || this
                        .terminal
                        .read(cx)
                        .prompt_context_request()
                        .map(|request| request.generation)
                        != Some(generation)
                {
                    return;
                }
                if let Some(ui) = &mut this.command_ui {
                    ui.prompt_context = context;
                    ui.prompt_task = None;
                    cx.notify();
                }
            })
            .log_err();
        }));
    }

    pub(super) fn command_editor_focus(&self, cx: &App) -> Option<FocusHandle> {
        let ui = self.command_ui.as_ref()?;
        (self.terminal.read(cx).command_input_state() == CommandInputState::Ready)
            .then(|| ui.input.focus_handle(cx))
    }

    pub(super) fn update_command_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        let text_style = terminal_text_style(cx);
        let input_style = gpui::TextStyleRefinement {
            font_family: Some(text_style.font_family),
            font_features: Some(text_style.font_features),
            font_fallbacks: text_style.font_fallbacks,
            font_size: Some(text_style.font_size),
            font_weight: Some(text_style.font_weight),
            font_style: Some(text_style.font_style),
            line_height: Some(text_style.line_height),
            color: Some(text_style.color),
            ..Default::default()
        };
        if ui.input_style.as_ref() != Some(&input_style) {
            ui.input.update(cx, |editor, cx| {
                editor.set_text_style_refinement(input_style.clone());
                cx.notify();
            });
            ui.input_style = Some(input_style);
        }
        let state = self.terminal.read(cx).command_input_state();
        let state_changed = ui.state != state;
        if state_changed {
            let had_focus = self.focus_handle.is_focused(window)
                || ui.input.focus_handle(cx).contains_focused(window, cx);
            ui.state = state;
            ui.input.update(cx, |editor, cx| {
                editor.set_read_only(state != CommandInputState::Ready);
                cx.notify();
            });
            if had_focus {
                if state == CommandInputState::Ready {
                    ui.input.focus_handle(cx).focus(window, cx);
                } else {
                    self.focus_handle.focus(window, cx);
                }
            }
        }
        if state_changed && state != CommandInputState::Ready {
            self.dismiss_command_suggestions(cx);
        }
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        let blocks = self.terminal.read(cx).command_blocks();
        if blocks.len() == ui.blocks.len()
            && blocks.last().map(|block| block.output.entity_id())
                == ui.blocks.last().map(|block| block.id)
        {
            return;
        }
        let blocks = blocks.to_vec();
        ui.blocks.retain(|view| {
            blocks
                .iter()
                .any(|block| block.output.entity_id() == view.id)
        });
        let follow = ui.scroll.offset().y <= -ui.scroll.max_offset().y + px(2.);
        for block in blocks {
            let id = block.output.entity_id();
            if ui.blocks.iter().any(|view| view.id == id) {
                continue;
            }
            let view = cx.new(|cx| {
                let mut view = TerminalView::new(
                    block.output.clone(),
                    self.workspace.clone(),
                    self.workspace_id,
                    self.project.clone(),
                    window,
                    cx,
                )
                .with_read_only(true);
                view.mode = TerminalMode::CommandOutput;
                view.set_show_workspace_actions(false, cx);
                view
            });
            ui.blocks.push(BlockView {
                id,
                view,
                block,
                collapsed: false,
            });
            if follow {
                ui.scroll.scroll_to_bottom();
            }
        }
    }

    fn submit_editor_command(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        let command = ui.input.read(cx).text(cx);
        if command.trim().is_empty() {
            return;
        }
        match self.terminal.update(cx, |terminal, cx| {
            terminal.submit_command(command.clone(), cx)
        }) {
            Ok(()) => {
                if ui.session_history.last() != Some(&command) {
                    ui.session_history.push(command.clone());
                }
                if ui.history.last() != Some(&command) {
                    ui.history.push(command);
                }
                let limit = self.terminal.read(cx).command_history_line_limit();
                if ui.session_history.len() > limit {
                    ui.session_history.drain(..ui.session_history.len() - limit);
                }
                if ui.history.len() > limit {
                    ui.history.drain(..ui.history.len() - limit);
                }
                ui.history_position = None;
                ui.draft.clear();
                ui.error = None;
                ui.input
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
                ui.scroll.scroll_to_bottom();
                self.focus_handle.focus(window, cx);
            }
            Err(error) => ui.error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn command_input_key(
        &mut self,
        key: &gpui::Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.command_ui.as_ref().is_some_and(|ui| {
            ui.input.update(cx, |editor, cx| {
                gpui::EntityInputHandler::marked_text_range(editor, window, cx).is_some()
            })
        }) {
            return;
        }
        if self.command_suggestion_key(key, window, cx) {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        let Some(ui) = &mut self.command_ui else {
            return;
        };
        if key.key == "enter"
            && !key.modifiers.shift
            && !key.modifiers.control
            && !key.modifiers.alt
            && !key.modifiers.platform
        {
            self.submit_editor_command(window, cx);
        } else if key.key == "c" && key.modifiers.control {
            ui.input
                .update(cx, |editor, cx| editor.set_text("", window, cx));
            ui.history_position = None;
            ui.draft.clear();
        } else if key.key == "l" && key.modifiers.control {
            self.terminal
                .update(cx, |terminal, cx| terminal.clear_command_blocks(cx));
            ui.blocks.clear();
        } else {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    pub(super) fn render_command_blocks(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        self.update_command_ui(window, cx);
        self.refresh_prompt_context(cx);
        self.refresh_command_history(window, cx);
        let suggestions = self.render_command_suggestions(window, cx);
        let Some(ui) = &self.command_ui else {
            return div().into_any_element();
        };
        let state = ui.state;
        let input = ui.input.clone();
        let error = ui.error.clone();
        let completion_error = ui.completion_error.clone();
        let history_error = ui.history_error.clone();
        let ready = state == CommandInputState::Ready;
        let running = state == CommandInputState::Running;
        let background = cx.theme().colors().terminal_background;
        let border = cx.theme().colors().border;
        let text_style = terminal_text_style(cx);
        let input_style = input.update(cx, |editor, cx| {
            let mut style = editor.style(cx).clone();
            style.text = text_style.clone();
            style
        });
        let input = editor::EditorElement::new(&input, input_style);
        let content_inset = window.pixel_snap(COMMAND_CONTENT_INSET.to_pixels(window.rem_size()));
        let line_height = text_style.line_height_in_pixels(window.rem_size());
        let header_height = line_height.max(IconSize::Small.square(window, cx))
            + rems(0.5).to_pixels(window.rem_size());
        let blocks = ui
            .blocks
            .iter()
            .map(|block| {
                let id = block.id;
                let output = block.block.output.clone();
                let command = block.block.command.clone();
                let status = match block.block.exit_code {
                    Some(0) => format!("{:.1}s", block.block.duration.as_secs_f64()),
                    Some(code) => {
                        format!("Exit {code} · {:.1}s", block.block.duration.as_secs_f64())
                    }
                    None => "Exit ?".into(),
                };
                let directory = block
                    .block
                    .directory
                    .as_ref()
                    .map(|path| path.compact().to_string_lossy().into_owned());
                v_flex()
                    .id(("command-block", id))
                    .group("command-block")
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .min_h(header_height)
                            .gap_2()
                            .px(content_inset)
                            .py_1()
                            .child(
                                div()
                                    .id(("command-label", id))
                                    .flex_1()
                                    .min_w_0()
                                    .tooltip(Tooltip::text(command.clone()))
                                    .font(text_style.font())
                                    .text_size(text_style.font_size)
                                    .line_height(text_style.line_height)
                                    .text_color(text_style.color)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(command.clone()),
                            )
                            .when_some(directory, |this, directory| {
                                this.child(
                                    div()
                                        .id(("command-directory", id))
                                        .min_w_0()
                                        .max_w(px(240.))
                                        .tooltip(Tooltip::text(directory.clone()))
                                        .child(
                                            Label::new(directory)
                                                .size(LabelSize::XSmall)
                                                .color(Color::Muted)
                                                .truncate(),
                                        ),
                                )
                            })
                            .child(
                                div()
                                    .id(("command-status", id))
                                    .flex_none()
                                    .when(block.block.exit_code.is_none(), |this| {
                                        this.tooltip(Tooltip::text(
                                            "Session ended; exit status unavailable",
                                        ))
                                    })
                                    .child(Label::new(status).size(LabelSize::XSmall).color(
                                        match block.block.exit_code {
                                            Some(0) => Color::Muted,
                                            Some(_) => Color::Error,
                                            None => Color::Warning,
                                        },
                                    )),
                            )
                            .child(
                                h_flex()
                                    .flex_none()
                                    .visible_on_hover("command-block")
                                    .child(
                                        IconButton::new(
                                            ("collapse", id),
                                            if block.collapsed {
                                                IconName::ChevronRight
                                            } else {
                                                IconName::ChevronDown
                                            },
                                        )
                                        .icon_size(IconSize::Small)
                                        .tooltip(Tooltip::text(if block.collapsed {
                                            "Expand output"
                                        } else {
                                            "Collapse output"
                                        }))
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                if let Some(ui) = &mut this.command_ui {
                                                    if let Some(block) = ui
                                                        .blocks
                                                        .iter_mut()
                                                        .find(|block| block.id == id)
                                                    {
                                                        block.collapsed = !block.collapsed;
                                                    }
                                                }
                                                cx.notify();
                                            }),
                                        ),
                                    )
                                    .child(
                                        IconButton::new(("copy-command", id), IconName::Copy)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Copy command"))
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    command.clone(),
                                                ))
                                            }),
                                    )
                                    .child(
                                        IconButton::new(("copy-output", id), IconName::Copy)
                                            .icon_size(IconSize::Small)
                                            .tooltip(Tooltip::text("Copy output"))
                                            .on_click(move |_, _, cx| {
                                                cx.write_to_clipboard(ClipboardItem::new_string(
                                                    output.read(cx).get_content(),
                                                ))
                                            }),
                                    ),
                            ),
                    )
                    .when(!block.collapsed, |this| this.child(block.view.clone()))
            })
            .collect::<Vec<_>>();
        let command = self.terminal.read(cx).running_command();
        let output_lines = self.terminal.read(cx).running_command_output_lines();
        let directory = if ui.prompt_context.directory.is_empty() {
            self.terminal
                .read(cx)
                .working_directory()
                .map(|path| path.display().to_string())
                .unwrap_or_default()
        } else {
            ui.prompt_context.directory.clone()
        };
        let tag = |id: String, label: String, icon: Option<IconName>, color: Color| {
            h_flex()
                .id(id)
                .min_w_0()
                .max_w_full()
                .gap_1()
                .px_2()
                .py_0p5()
                .rounded_md()
                .border_1()
                .border_color(border)
                .bg(cx.theme().colors().element_background)
                .tooltip(Tooltip::text(label.clone()))
                .when_some(icon, |this, icon| {
                    this.child(
                        Icon::new(icon)
                            .morph_id("command-status")
                            .size(IconSize::XSmall)
                            .color(color),
                    )
                })
                .child(
                    div()
                        .min_w_0()
                        .font(text_style.font())
                        .text_size(text_style.font_size)
                        .line_height(text_style.line_height)
                        .text_color(color.color(cx))
                        .whitespace_nowrap()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(label),
                )
        };
        let context_bar = h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .child(tag(
                "prompt-directory".into(),
                directory,
                Some(IconName::Folder),
                Color::Custom(cx.theme().colors().terminal_ansi_blue),
            ))
            .when_some(ui.prompt_context.branch.clone(), |this, branch| {
                this.child(tag(
                    "prompt-branch".into(),
                    branch,
                    Some(IconName::GitBranch),
                    Color::Custom(cx.theme().colors().terminal_ansi_magenta),
                ))
            })
            .children(
                ui.prompt_context
                    .runtimes
                    .iter()
                    .enumerate()
                    .map(|(index, runtime)| {
                        tag(
                            format!("prompt-runtime-{index}"),
                            runtime.clone(),
                            None,
                            Color::Custom(cx.theme().colors().terminal_ansi_green),
                        )
                    }),
            )
            .when(!ui.prompt_context.errors.is_empty(), |this| {
                this.child(
                    IconButton::new("prompt-context-error", IconName::Info)
                        .icon_size(IconSize::XSmall)
                        .tooltip(Tooltip::text(ui.prompt_context.errors.join("\n"))),
                )
            });
        let focused = self.focus_handle.is_focused(window);
        v_flex()
            .id("terminal-command-blocks")
            .size_full()
            .bg(background)
            .track_focus(&self.focus_handle)
            .when(!ready, |this| {
                this.key_context(self.dispatch_context(cx))
                    .on_key_down(cx.listener(Self::key_down))
                    .on_action(cx.listener(Self::paste))
                    .on_action(cx.listener(Self::editor_paste))
                    .on_action(cx.listener(Self::copy))
                    .on_action(cx.listener(Self::editor_copy))
                    .on_action(cx.listener(Self::send_text))
                    .on_action(cx.listener(Self::send_keystroke))
            })
            .on_action(cx.listener(Self::clear))
            .child(
                v_flex()
                    .id("command-history")
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&ui.scroll)
                    .children(blocks)
                    .when(running || state == CommandInputState::Starting, |this| {
                        this.child(
                            v_flex()
                                .w_full()
                                .flex_none()
                                .border_b_1()
                                .border_color(cx.theme().colors().border_variant)
                                .child(
                                    h_flex()
                                        .w_full()
                                        .min_w_0()
                                        .min_h(header_height)
                                        .px(content_inset)
                                        .py_1()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .font(text_style.font())
                                                .text_size(text_style.font_size)
                                                .line_height(text_style.line_height)
                                                .text_color(text_style.color)
                                                .whitespace_nowrap()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .child(command.unwrap_or_else(|| {
                                                    if state == CommandInputState::Starting {
                                                        "Waiting for shell…".into()
                                                    } else {
                                                        "Terminal input".into()
                                                    }
                                                })),
                                        )
                                        .when(running, |this| {
                                            this.child(
                                                Label::new("Running")
                                                    .size(LabelSize::XSmall)
                                                    .color(Color::Muted),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .w_full()
                                        .h(line_height * output_lines.clamp(1, 18) as f32)
                                        .overflow_hidden()
                                        .child(
                                            // Keep the PTY viewport stable while revealing only its used rows.
                                            div()
                                                .w_full()
                                                .h(line_height * 18.)
                                                .when(output_lines == 0, |this| this.invisible())
                                                .child(TerminalElement::new(
                                                    self.terminal.clone(),
                                                    cx.entity(),
                                                    self.workspace.clone(),
                                                    self.focus_handle.clone(),
                                                    focused,
                                                    self.should_show_cursor(focused, cx),
                                                    None,
                                                    TerminalMode::Standalone,
                                                )),
                                        ),
                                ),
                        )
                    })
                    .when(state == CommandInputState::Exited, |this| {
                        this.child(
                            Label::new("Shell exited. Open a new terminal to continue.")
                                .color(Color::Muted),
                        )
                    })
                    .when(ready || running, |this| {
                        this.child(
                            v_flex()
                                .id("terminal-command-input")
                                .flex_none()
                                .w_full()
                                // Preserve its layout while running so restoring the
                                // prompt does not move the output or the scroll anchor.
                                .when(!ready, |this| this.invisible())
                                .px(content_inset)
                                .py_2()
                                .gap_2()
                                .child(context_bar)
                                .child(
                                    div()
                                        .relative()
                                        .w_full()
                                        .min_w_0()
                                        .child(input)
                                        .child(suggestions),
                                )
                                .when_some(history_error, |this, error| {
                                    this.child(
                                        Label::new(error)
                                            .size(LabelSize::Small)
                                            .color(Color::Error),
                                    )
                                })
                                .when_some(completion_error, |this, error| {
                                    this.child(
                                        Label::new(error)
                                            .size(LabelSize::Small)
                                            .color(Color::Error),
                                    )
                                })
                                .when_some(error, |this, error| {
                                    this.child(Label::new(error).color(Color::Error))
                                }),
                        )
                    }),
            )
            .into_any_element()
    }
}
