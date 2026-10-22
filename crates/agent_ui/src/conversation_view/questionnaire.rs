use agent::AskUserQuestionnaire;
use agent_client_protocol::schema::v2 as acp;
use collections::HashSet;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement,
    ParentElement, Render, ScrollHandle, Styled, Subscription, Window, black, div, prelude::*,
    rems,
};
use gpui_kit::{
    base::{h_flex, v_flex},
    component::{
        Disableable, Icon, Sizable, Theme,
        button::{Button, ButtonVariants},
        input::{Input, InputEvent, InputState},
    },
};
use markdown::{Markdown, MarkdownElement, MarkdownFont, MarkdownStyle};
use std::collections::BTreeMap;

pub(super) struct Questionnaire {
    form: AskUserQuestionnaire,
    questions: Vec<QuestionState>,
    current: usize,
    highlighted_option: usize,
    collapsed: bool,
    submitted: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _subscriptions: Vec<Subscription>,
}

struct QuestionState {
    prompt: Entity<Markdown>,
    selected: HashSet<String>,
    other_selected: bool,
    other: Entity<InputState>,
}

impl EventEmitter<acp::CreateElicitationResponse> for Questionnaire {}

impl Focusable for Questionnaire {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Questionnaire {
    pub(super) fn new(
        form: AskUserQuestionnaire,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut subscriptions = Vec::new();
        let questions = form
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let other =
                    cx.new(|cx| InputState::new(window, cx).placeholder("Type your answer…"));
                subscriptions.push(cx.subscribe_in(
                    &other,
                    window,
                    move |this, _, event, window, cx| {
                        if matches!(event, InputEvent::Focus) {
                            this.current = index;
                        }
                        if matches!(
                            event,
                            InputEvent::PressEnter {
                                secondary: false,
                                shift: false
                            }
                        ) {
                            this.next_or_submit(window, cx);
                        }
                        cx.notify();
                    },
                ));
                QuestionState {
                    prompt: cx
                        .new(|cx| Markdown::new(question.prompt.clone().into(), None, None, cx)),
                    selected: HashSet::default(),
                    other_selected: question.options.is_empty() && form.allow_other,
                    other,
                }
            })
            .collect();
        Self {
            form,
            questions,
            current: 0,
            highlighted_option: 0,
            collapsed: false,
            submitted: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        }
    }

    fn answered(&self, index: usize, cx: &App) -> bool {
        self.questions.get(index).is_some_and(|question| {
            (!question.selected.is_empty() || question.other_selected)
                && (!question.other_selected || !question.other.read(cx).value().trim().is_empty())
        })
    }

    fn navigate(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.questions.len() {
            self.current = index;
            self.highlighted_option = 0;
            self.scroll.scroll_to_top_of_item(index);
            cx.notify();
        }
    }

    fn choose(
        &mut self,
        question_index: usize,
        option_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.submitted {
            return;
        }
        let Some(question) = self.form.questions.get(question_index) else {
            return;
        };
        let Some(state) = self.questions.get_mut(question_index) else {
            return;
        };
        self.current = question_index;
        self.highlighted_option = option_index;
        if let Some(option) = question.options.get(option_index) {
            if question.allow_multiple {
                if !state.selected.remove(&option.id) {
                    state.selected.insert(option.id.clone());
                }
            } else {
                state.selected.clear();
                state.selected.insert(option.id.clone());
                state.other_selected = false;
                if let Some(next) = (question_index + 1..self.questions.len())
                    .find(|index| !self.answered(*index, cx))
                {
                    self.navigate(next, cx);
                }
                self.focus.focus(window, cx);
            }
        } else if self.form.allow_other {
            state.other_selected = !question.allow_multiple || !state.other_selected;
            if !question.allow_multiple {
                state.selected.clear();
            }
            if state.other_selected {
                state.other.focus_handle(cx).focus(window, cx);
            }
        }
        cx.notify();
    }

    fn next_or_submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitted || !self.answered(self.current, cx) {
            return;
        }
        if let Some(next) = (self.current + 1..self.questions.len())
            .chain(0..self.current)
            .find(|index| !self.answered(*index, cx))
        {
            self.navigate(next, cx);
            self.focus.focus(window, cx);
            return;
        }
        let mut content = BTreeMap::new();
        for (index, (question, state)) in
            self.form.questions.iter().zip(&self.questions).enumerate()
        {
            let selected = question
                .options
                .iter()
                .filter(|option| state.selected.contains(&option.id))
                .map(|option| option.id.clone())
                .collect::<Vec<_>>();
            if question.allow_multiple {
                content.insert(
                    self.form.choice_field(index),
                    acp::ElicitationContentValue::StringArray(selected),
                );
            } else if let Some(selected) = selected.first() {
                content.insert(
                    self.form.choice_field(index),
                    acp::ElicitationContentValue::String(selected.clone()),
                );
            }
            if state.other_selected {
                content.insert(
                    self.form.other_field(index),
                    acp::ElicitationContentValue::String(
                        state.other.read(cx).value().trim().to_owned(),
                    ),
                );
            }
        }
        self.submitted = true;
        cx.emit(acp::CreateElicitationResponse::new(
            acp::ElicitationAction::Accept(acp::ElicitationAcceptAction::new().content(content)),
        ));
        cx.notify();
    }
}

impl Render for Questionnaire {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::global(cx);
        let foreground = theme.foreground;
        let muted = theme.muted_foreground;
        let highlight = foreground.opacity(0.07);
        let badge_background = foreground.opacity(0.10);
        let selected_badge = theme.warning;
        let selected_foreground = black();
        let title = if self.questions.len() == 1 {
            "Question".to_owned()
        } else {
            format!("Questions ({})", self.questions.len())
        };
        let all_answered = (0..self.questions.len()).all(|index| self.answered(index, cx));
        let can_continue = self.answered(self.current, cx) && !self.submitted;
        let current = self.current;
        let rows = self
            .form
            .questions
            .iter()
            .zip(&self.questions)
            .enumerate()
            .map(|(question_index, (question, state))| {
                let options = question
                    .options
                    .iter()
                    .map(|option| (option.label.clone(), state.selected.contains(&option.id)))
                    .chain(
                        self.form
                            .allow_other
                            .then(|| ("Other…".to_owned(), state.other_selected)),
                    )
                    .enumerate()
                    .map(|(option_index, (label, selected))| {
                        let badge = if option_index < 26 {
                            char::from(b'A' + option_index as u8).to_string()
                        } else {
                            (option_index + 1).to_string()
                        };
                        let is_other = option_index == question.options.len();
                        let badge = div()
                            .flex_none()
                            .size(rems(1.25))
                            .rounded(rems(0.25))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(rems(0.75))
                            .font_weight(FontWeight::MEDIUM)
                            .bg(if selected {
                                selected_badge
                            } else {
                                badge_background
                            })
                            .text_color(if selected { selected_foreground } else { muted })
                            .child(badge);
                        if is_other && state.other_selected {
                            return h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .rounded(rems(0.5))
                                .px_2()
                                .py_1p5()
                                .bg(highlight)
                                .child(
                                    Button::new(("question-option", option_index))
                                        .ghost()
                                        .size(rems(1.25))
                                        .p_0()
                                        .rounded(rems(0.25).to_pixels(window.rem_size()))
                                        .tooltip("Other answer")
                                        .disabled(self.submitted)
                                        .child(badge)
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            this.choose(question_index, option_index, window, cx)
                                        })),
                                )
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&state.other)
                                            .small()
                                            .appearance(false)
                                            .p_0()
                                            .disabled(self.submitted),
                                    ),
                                )
                                .into_any_element();
                        }
                        Button::new(("question-option", option_index))
                            .ghost()
                            .w_full()
                            .h_auto()
                            .justify_start()
                            .rounded(rems(0.5).to_pixels(window.rem_size()))
                            .px_2()
                            .py_1p5()
                            .when(
                                selected
                                    || (current == question_index
                                        && self.highlighted_option == option_index),
                                |button| button.bg(highlight),
                            )
                            .disabled(self.submitted)
                            .child(
                                h_flex().w_full().gap_2().child(badge).child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .whitespace_normal()
                                        .text_color(foreground)
                                        .child(label),
                                ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.choose(question_index, option_index, window, cx)
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>();
                v_flex()
                    .id(("question", question_index))
                    .w_full()
                    .gap_0p5()
                    .pb_2()
                    .opacity(if current == question_index { 1. } else { 0.7 })
                    .child(div().px_2().pt_1p5().pb_2().child(MarkdownElement::new(
                        state.prompt.clone(),
                        MarkdownStyle::themed(MarkdownFont::Agent, window, cx),
                    )))
                    .children(options)
            })
            .collect::<Vec<_>>();
        v_flex()
            .id("questionnaire")
            .w_full()
            .min_w_0()
            .rounded(rems(0.5))
            .border_1()
            .border_color(foreground.opacity(0.2))
            .bg(theme.background)
            .text_color(foreground)
            .track_focus(&self.focus)
            .key_context("Elicitation")
            .tab_group()
            .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, window, cx| {
                if this.submitted
                    || event.keystroke.modifiers.platform
                    || event.keystroke.modifiers.control
                    || event.keystroke.modifiers.alt
                {
                    return;
                }
                let editing = this
                    .questions
                    .iter()
                    .any(|question| question.other.focus_handle(cx).contains_focused(window, cx));
                if event.keystroke.key == "escape" {
                    this.collapsed = true;
                    this.focus.focus(window, cx);
                } else if editing || this.collapsed {
                    return;
                } else {
                    match event.keystroke.key.as_str() {
                        "left" => this.navigate(this.current.saturating_sub(1), cx),
                        "right" => this.navigate(this.current + 1, cx),
                        "up" => this.highlighted_option = this.highlighted_option.saturating_sub(1),
                        "down" => {
                            let count =
                                this.form.questions.get(this.current).map_or(0, |question| {
                                    question.options.len() + usize::from(this.form.allow_other)
                                });
                            this.highlighted_option =
                                (this.highlighted_option + 1).min(count.saturating_sub(1));
                        }
                        "space" => this.choose(this.current, this.highlighted_option, window, cx),
                        "enter" => this.next_or_submit(window, cx),
                        key if key.len() == 1
                            && key.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) =>
                        {
                            let index = usize::from(
                                key.as_bytes()
                                    .first()
                                    .copied()
                                    .unwrap_or(b'a')
                                    .to_ascii_lowercase()
                                    - b'a',
                            );
                            if this
                                .form
                                .questions
                                .get(this.current)
                                .is_some_and(|question| {
                                    index
                                        < question.options.len()
                                            + usize::from(this.form.allow_other)
                                })
                            {
                                this.choose(this.current, index, window, cx);
                            } else {
                                return;
                            }
                        }
                        _ => return,
                    }
                }
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                h_flex().px_3().py_2().justify_between().child(title).child(
                    Button::new("collapse-questions")
                        .ghost()
                        .small()
                        .icon(Icon::empty().path(if self.collapsed {
                            "icons/chevron_up.svg"
                        } else {
                            "icons/chevron_down.svg"
                        }))
                        .tooltip(if self.collapsed {
                            "Expand questions"
                        } else {
                            "Collapse questions"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.collapsed = !this.collapsed;
                            cx.notify();
                        })),
                ),
            )
            .when(!self.collapsed, |tray| {
                tray.child(
                    v_flex()
                        .id("questionnaire-scroll")
                        .track_scroll(&self.scroll)
                        .w_full()
                        .min_h_0()
                        .max_h(rems(20.))
                        .overflow_y_scroll()
                        .px_1()
                        .children(rows),
                )
                .child(
                    h_flex()
                        .p_3()
                        .gap_1p5()
                        .justify_between()
                        .child(
                            h_flex()
                                .gap_1p5()
                                .child(
                                    Button::new("previous-question")
                                        .ghost()
                                        .small()
                                        .label("‹")
                                        .tooltip("Previous question")
                                        .disabled(current == 0 || self.submitted)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.navigate(this.current.saturating_sub(1), cx)
                                        })),
                                )
                                .child(
                                    div().text_size(rems(0.75)).text_color(muted).child(format!(
                                        "{} of {}",
                                        current + 1,
                                        self.questions.len()
                                    )),
                                )
                                .child(
                                    Button::new("next-question")
                                        .ghost()
                                        .small()
                                        .label("›")
                                        .tooltip("Next question")
                                        .disabled(
                                            current + 1 >= self.questions.len() || self.submitted,
                                        )
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.navigate(this.current + 1, cx)
                                        })),
                                ),
                        )
                        .child(
                            h_flex()
                                .gap_1p5()
                                .child(
                                    Button::new("skip-questions")
                                        .ghost()
                                        .small()
                                        .label("Skip")
                                        .disabled(self.submitted)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            if !this.submitted {
                                                this.submitted = true;
                                                cx.emit(acp::CreateElicitationResponse::new(
                                                    acp::ElicitationAction::Decline,
                                                ));
                                                cx.notify();
                                            }
                                        })),
                                )
                                .child(
                                    Button::new("continue-questions")
                                        .primary()
                                        .small()
                                        .label(if all_answered { "Continue" } else { "Next" })
                                        .disabled(!can_continue)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.next_or_submit(window, cx)
                                        })),
                                ),
                        ),
                )
            })
    }
}
