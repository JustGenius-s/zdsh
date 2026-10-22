use super::ThreadView;
use crate::{ScrollOutputLineDown, ScrollOutputLineUp};
use acp_thread::{AgentThreadEntry, AssistantMessageChunk};
use agent_settings::AgentSettings;
use gpui_kit::base::{
    Button, ElementExt as _,
    motion::{Transition, transition},
    v_flex,
};
use gpui_kit::component::{Theme, tooltip::Tooltip};
use gpui_kit::{
    App, Bounds, ClickEvent, Context, ElementId, FocusHandle, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseMoveEvent, MouseUpEvent,
    ParentElement as _, Pixels, Point, StatefulInteractiveElement as _, Styled as _, Window,
    anchored, canvas, deferred, div, point, prelude::FluentBuilder as _, px, relative,
};
use markdown::{
    Markdown,
    parser::{MarkdownEvent, MarkdownTag, MarkdownTagEnd},
};
use settings::Settings as _;
use std::time::Duration;
use theme_settings::ThemeSettings;
use util::ResultExt as _;

const RAIL_WIDTH: f32 = 52.;
const ROW_HEIGHT: f32 = 10.;
const RAIL_PADDING: f32 = 4.;
const MAX_VISIBLE_TURNS: usize = 31;

pub(super) struct NavigationState {
    focus_handle: FocusHandle,
    turns: Vec<NavigationTurn>,
    bounds: Option<Bounds<Pixels>>,
    hovered_turn: Option<usize>,
    drag: Option<NavigationDrag>,
    pub(super) top_entry: usize,
}

struct NavigationTurn {
    entry_index: usize,
    focus_handle: FocusHandle,
}

#[derive(Clone, Copy)]
struct NavigationDrag {
    start_turn: usize,
    first_row_top: Pixels,
    selected_turn: usize,
}

impl NavigationDrag {
    fn turn_at(&self, position: Point<Pixels>, turn_count: usize) -> usize {
        let row = ((position.y - self.first_row_top) / px(ROW_HEIGHT)).floor();
        (self.start_turn as f32 + row).clamp(0., turn_count.saturating_sub(1) as f32) as usize
    }
}

impl NavigationState {
    pub(super) fn new(cx: &mut App) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            turns: Vec::new(),
            bounds: None,
            hovered_turn: None,
            drag: None,
            top_entry: 0,
        }
    }
}

impl ThreadView {
    pub(super) fn subscribe_to_navigation_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus_handle = &self.conversation_navigation.focus_handle;
        self._subscriptions.extend([
            cx.on_focus_in(focus_handle, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(focus_handle, window, |this, _, _, cx| {
                this.conversation_navigation.hovered_turn = None;
                this.conversation_navigation.drag = None;
                cx.notify();
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if !window.is_window_active() {
                    this.conversation_navigation.hovered_turn = None;
                    this.conversation_navigation.drag = None;
                    cx.notify();
                }
            }),
        ]);
    }

    pub(super) fn render_conversation_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entry_indices = self
            .thread
            .read(cx)
            .entries()
            .iter()
            .enumerate()
            .filter_map(|(entry_index, entry)| match entry {
                AgentThreadEntry::UserMessage(message)
                    if !message.indented && !message.content.source_blocks().is_empty() =>
                {
                    Some(entry_index)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        let navigation = &mut self.conversation_navigation;
        if !navigation
            .turns
            .iter()
            .map(|turn| turn.entry_index)
            .eq(entry_indices.iter().copied())
        {
            let mut previous_turns = std::mem::take(&mut navigation.turns).into_iter().peekable();
            navigation.turns = entry_indices
                .into_iter()
                .map(|entry_index| {
                    while previous_turns
                        .peek()
                        .is_some_and(|turn| turn.entry_index < entry_index)
                    {
                        previous_turns.next();
                    }
                    if previous_turns
                        .peek()
                        .is_some_and(|turn| turn.entry_index == entry_index)
                        && let Some(turn) = previous_turns.next()
                    {
                        return turn;
                    }
                    NavigationTurn {
                        entry_index,
                        focus_handle: cx.focus_handle(),
                    }
                })
                .collect();
            navigation.hovered_turn = None;
            navigation.drag = None;
        }
        if !window.is_window_active() {
            navigation.hovered_turn = None;
            navigation.drag = None;
        }
        navigation.top_entry = self.list_state.logical_scroll_top().item_ix;

        let view = cx.weak_entity();
        let overlay =
            div()
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .on_prepaint(move |bounds, _, cx| {
                    // Notifications during prepaint do not schedule another frame.
                    cx.defer(move |cx| {
                        view.update(cx, |this, cx| {
                            if this.conversation_navigation.bounds != Some(bounds) {
                                this.conversation_navigation.bounds = Some(bounds);
                                this.conversation_navigation.drag = None;
                                this.conversation_navigation.hovered_turn = None;
                                cx.notify();
                            }
                        })
                        .log_err();
                    });
                });
        let Some(bounds) = navigation.bounds else {
            return overlay;
        };
        let max_content_width = AgentSettings::get_global(cx).max_content_width;
        let gutter_width = max_content_width
            .map(|width| (bounds.size.width - width).max(px(0.)) / 2.)
            .unwrap_or_default()
            + super::transcript::COLUMN_INSET;
        let turn_count = navigation.turns.len();
        let rail_height = (bounds.size.height * 0.5).min(px(320.));
        if turn_count < 2
            || gutter_width < px(RAIL_WIDTH)
            || rail_height < px(ROW_HEIGHT + RAIL_PADDING * 2.)
        {
            navigation.hovered_turn = None;
            navigation.drag = None;
            return overlay;
        }

        let visible_count = (((rail_height - px(RAIL_PADDING * 2.)) / px(ROW_HEIGHT)) as usize)
            .min(MAX_VISIBLE_TURNS)
            .min(turn_count);
        let current_turn = navigation
            .turns
            .partition_point(|turn| turn.entry_index <= navigation.top_entry)
            .saturating_sub(1);
        let focused_turn = navigation
            .turns
            .iter()
            .position(|turn| turn.focus_handle.is_focused(window));
        let preview_turn = navigation
            .drag
            .map(|drag| drag.selected_turn)
            .or(navigation.hovered_turn)
            .or(focused_turn)
            .filter(|_| window.is_window_active());
        let anchor_turn = focused_turn.unwrap_or(current_turn);
        let mut first_turn = anchor_turn
            .saturating_sub(visible_count / 2)
            .min(turn_count - visible_count);
        if let Some(drag) = navigation.drag {
            first_turn = drag
                .start_turn
                .min(drag.selected_turn)
                .max(drag.selected_turn.saturating_sub(visible_count - 1))
                .min(turn_count - visible_count);
        }
        let rail_top =
            (bounds.size.height - px(visible_count as f32 * ROW_HEIGHT + RAIL_PADDING * 2.)) / 2.;
        let first_row_top = bounds.top() + rail_top + px(RAIL_PADDING);
        let foreground = Theme::global(cx).foreground;
        let mut rail = v_flex()
            .id("conversation-navigation")
            .track_focus(&navigation.focus_handle)
            .absolute()
            .left_0()
            .top(rail_top)
            .w(px(RAIL_WIDTH))
            .py(px(RAIL_PADDING))
            .on_key_down(cx.listener(Self::navigate_conversation_with_keyboard))
            .on_action(cx.listener(|this, _: &ScrollOutputLineUp, window, cx| {
                this.focus_adjacent_navigation_turn(false, window, cx);
            }))
            .on_action(cx.listener(|this, _: &ScrollOutputLineDown, window, cx| {
                this.focus_adjacent_navigation_turn(true, window, cx);
            }))
            .on_action(cx.listener(|this, _: &menu::Confirm, window, cx| {
                if let Some(turn_index) = this
                    .conversation_navigation
                    .turns
                    .iter()
                    .position(|turn| turn.focus_handle.is_focused(window))
                {
                    this.jump_to_navigation_turn(turn_index, cx);
                }
            }));
        for (turn_index, turn) in navigation
            .turns
            .iter()
            .enumerate()
            .skip(first_turn)
            .take(visible_count)
        {
            let width = match preview_turn.map(|preview| turn_index.abs_diff(preview)) {
                Some(0) => 20.,
                Some(1) => 14.,
                Some(2) => 10.,
                Some(3) => 8.,
                _ => 6.,
            };
            let width = transition(
                ElementId::from(("conversation-navigation-dash", turn.entry_index)),
                width,
                Transition::new(Duration::from_millis(100))
                    .ease(|progress| 1. - (1. - progress).powi(3)),
                window,
                cx,
            );
            let is_current = turn_index == current_turn;
            rail = rail.child(
                Button::new(("conversation-navigation-turn", turn.entry_index))
                    .track_focus(&turn.focus_handle)
                    .tab_stop(turn_index == anchor_turn)
                    .accessibility_label(format!(
                        "Turn {} of {}{}",
                        turn_index + 1,
                        turn_count,
                        if is_current { ", current turn" } else { "" }
                    ))
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .flex_shrink_0()
                    .justify_start()
                    .pl(px(12.))
                    .cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered, _, cx| {
                        let navigation = &mut this.conversation_navigation;
                        if navigation.drag.is_none() {
                            if *hovered {
                                navigation.hovered_turn = Some(turn_index);
                            } else if navigation.hovered_turn == Some(turn_index) {
                                navigation.hovered_turn = None;
                            }
                            cx.notify();
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            window.prevent_default();
                            cx.stop_propagation();
                            this.conversation_navigation.drag = Some(NavigationDrag {
                                start_turn: first_turn,
                                first_row_top,
                                selected_turn: turn_index,
                            });
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, event, _, cx| {
                        if !matches!(event, ClickEvent::Mouse(_)) {
                            this.jump_to_navigation_turn(turn_index, cx);
                        }
                    }))
                    .child(div().w(px(width)).h(px(2.)).bg(foreground).opacity(
                        if is_current || preview_turn == Some(turn_index) {
                            1.
                        } else {
                            0.28
                        },
                    )),
            );
        }
        let mut overlay = overlay.child(rail);
        if let Some(turn_index) =
            preview_turn.filter(|index| (first_turn..first_turn + visible_count).contains(index))
        {
            let (prompt, reply) = self.navigation_preview(turn_index, cx);
            let theme_settings = ThemeSettings::get_global(cx);
            let font_size = theme_settings.agent_ui_font_size(cx) * (12. / 14.);
            let font_family = theme_settings.agent_ui_font_family().clone();
            let tooltip = Tooltip::element(move |_, cx| {
                v_flex()
                    .max_w(px(280.))
                    .gap(px(4.))
                    .child(
                        div()
                            .line_clamp(3)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(Theme::global(cx).foreground)
                            .child(prompt.clone()),
                    )
                    .when(!reply.is_empty(), |this| {
                        this.child(
                            div()
                                .line_clamp(3)
                                .text_color(Theme::global(cx).muted_foreground)
                                .child(reply.clone()),
                        )
                    })
            })
            .m_0()
            .px(px(8.))
            .py(px(6.))
            .rounded(px(8.))
            .font_family(font_family)
            .text_size(font_size)
            .line_height(relative(4. / 3.))
            .build(window, cx);
            overlay = overlay.child(
                deferred(
                    anchored()
                        .position(point(
                            bounds.left() + px(RAIL_WIDTH),
                            first_row_top + px((turn_index - first_turn) as f32 * ROW_HEIGHT),
                        ))
                        .snap_to_window_with_margin(px(8.))
                        .child(tooltip),
                )
                .with_priority(1),
            );
        }

        let view = cx.weak_entity();
        overlay.child(
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    // The drag must still finish when the pointer leaves the rail's hit boxes.
                    window.on_mouse_event::<MouseMoveEvent>({
                        let view = view.clone();
                        move |event, phase, _, cx| {
                            if !phase.capture() {
                                return;
                            }
                            view.update(cx, |this, cx| {
                                let navigation = &mut this.conversation_navigation;
                                if let Some(drag) = navigation.drag.as_mut() {
                                    if event.pressed_button != Some(MouseButton::Left) {
                                        navigation.drag = None;
                                        navigation.hovered_turn = None;
                                        cx.notify();
                                        return;
                                    }
                                    let selected_turn =
                                        drag.turn_at(event.position, navigation.turns.len());
                                    if drag.selected_turn != selected_turn {
                                        drag.selected_turn = selected_turn;
                                        cx.notify();
                                    }
                                    cx.stop_propagation();
                                }
                            })
                            .log_err();
                        }
                    });
                    window.on_mouse_event::<MouseUpEvent>(move |event, phase, _, cx| {
                        if !phase.capture() || event.button != MouseButton::Left {
                            return;
                        }
                        view.update(cx, |this, cx| {
                            if let Some(drag) = this.conversation_navigation.drag.take() {
                                let selected_turn = drag.turn_at(
                                    event.position,
                                    this.conversation_navigation.turns.len(),
                                );
                                this.conversation_navigation.hovered_turn = None;
                                this.jump_to_navigation_turn(selected_turn, cx);
                                cx.stop_propagation();
                            }
                        })
                        .log_err();
                    });
                },
            )
            .absolute()
            .size_full(),
        )
    }

    fn navigate_conversation_with_keyboard(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "up" => self.focus_adjacent_navigation_turn(false, window, cx),
            "down" => self.focus_adjacent_navigation_turn(true, window, cx),
            "escape" => {
                if self.dismiss_conversation_navigation(window, cx) {
                    cx.stop_propagation();
                }
            }
            _ => {}
        }
    }

    fn focus_adjacent_navigation_turn(
        &mut self,
        next: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let navigation = &self.conversation_navigation;
        let Some(focused_turn) = navigation
            .turns
            .iter()
            .position(|turn| turn.focus_handle.is_focused(window))
        else {
            return;
        };
        let selected_turn = if next {
            (focused_turn + 1).min(navigation.turns.len().saturating_sub(1))
        } else {
            focused_turn.saturating_sub(1)
        };
        if let Some(turn) = navigation.turns.get(selected_turn) {
            turn.focus_handle.focus(window, cx);
            self.conversation_navigation.hovered_turn = None;
            cx.notify();
            cx.stop_propagation();
        }
    }

    pub(super) fn dismiss_conversation_navigation(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let navigation = &mut self.conversation_navigation;
        let focused = navigation.focus_handle.contains_focused(window, cx);
        if navigation.drag.is_none() && !focused {
            return false;
        }
        navigation.drag = None;
        navigation.hovered_turn = None;
        if focused {
            self.active_editor(cx).focus_handle(cx).focus(window, cx);
        }
        cx.notify();
        true
    }

    fn jump_to_navigation_turn(&mut self, turn_index: usize, cx: &mut Context<Self>) {
        if let Some(turn) = self.conversation_navigation.turns.get(turn_index) {
            self.scroll_to_user_message_index(Some(turn.entry_index), cx);
        }
    }

    fn navigation_preview(&self, turn_index: usize, cx: &App) -> (String, String) {
        let navigation = &self.conversation_navigation;
        let Some(turn) = navigation.turns.get(turn_index) else {
            return (String::new(), String::new());
        };
        let entries = self.thread.read(cx).entries();
        let prompt = match entries.get(turn.entry_index) {
            Some(AgentThreadEntry::UserMessage(message)) => message
                .content
                .markdowns()
                .map(|markdown| markdown_preview(markdown.read(cx), 280, false))
                .find(|preview| !preview.is_empty()),
            _ => None,
        }
        .unwrap_or_else(|| format!("Message {}", turn_index + 1));
        let end = navigation
            .turns
            .get(turn_index + 1)
            .map(|turn| turn.entry_index)
            .unwrap_or(entries.len());
        let reply = entries
            .get(turn.entry_index + 1..end)
            .into_iter()
            .flatten()
            .rev()
            .find_map(|entry| {
                let AgentThreadEntry::AssistantMessage(message) = entry else {
                    return None;
                };
                if message.indented || message.is_subagent_output {
                    return None;
                }
                message.chunks.iter().rev().find_map(|chunk| {
                    let AssistantMessageChunk::Message { block, .. } = chunk else {
                        return None;
                    };
                    block.markdowns().last()
                })
            })
            .map(|markdown| markdown_preview(markdown.read(cx), 160, true))
            .unwrap_or_default();
        (prompt, reply)
    }
}

fn markdown_preview(markdown: &Markdown, limit: usize, first_line: bool) -> String {
    let parsed = markdown.parsed_markdown();
    let mut preview = String::new();
    let mut length = 0;
    let mut skipped_depth = 0;
    for (range, event) in parsed.events().iter() {
        if skipped_depth > 0 {
            match event {
                MarkdownEvent::Start(_) => skipped_depth += 1,
                MarkdownEvent::End(_) => skipped_depth -= 1,
                _ => {}
            }
            continue;
        }
        let text = match event {
            MarkdownEvent::Start(MarkdownTag::HtmlBlock | MarkdownTag::MetadataBlock(_)) => {
                skipped_depth = 1;
                continue;
            }
            MarkdownEvent::Start(MarkdownTag::CodeBlock { .. }) if first_line => {
                skipped_depth = 1;
                continue;
            }
            MarkdownEvent::Text | MarkdownEvent::Code => {
                parsed.source().get(range.clone()).unwrap_or_default()
            }
            MarkdownEvent::SubstitutedText(text) | MarkdownEvent::SubstitutedCode(text) => text,
            MarkdownEvent::InlineMath(text) | MarkdownEvent::DisplayMath(text) => text,
            MarkdownEvent::SoftBreak
            | MarkdownEvent::HardBreak
            | MarkdownEvent::RootEnd(_)
            | MarkdownEvent::End(
                MarkdownTagEnd::Paragraph
                | MarkdownTagEnd::Heading(_)
                | MarkdownTagEnd::Item
                | MarkdownTagEnd::TableRow,
            ) => {
                if first_line && !preview.trim().is_empty() {
                    break;
                }
                " "
            }
            MarkdownEvent::End(MarkdownTagEnd::TableCell) => " ",
            _ => continue,
        };
        if !append_preview_text(&mut preview, &mut length, text, limit) {
            break;
        }
    }
    if preview.is_empty() && !first_line && parsed.events().is_empty() {
        append_preview_text(&mut preview, &mut length, markdown.source(), limit);
    }
    preview.trim().to_owned()
}

fn append_preview_text(preview: &mut String, length: &mut usize, text: &str, limit: usize) -> bool {
    for character in text.chars() {
        let character = if character.is_whitespace() {
            ' '
        } else {
            character
        };
        if character == ' ' && (preview.is_empty() || preview.ends_with(' ')) {
            continue;
        }
        if *length == limit {
            preview.push('…');
            return false;
        }
        preview.push(character);
        *length += 1;
    }
    true
}
