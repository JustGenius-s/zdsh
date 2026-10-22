use std::collections::BTreeMap;
use std::sync::Arc;

use crate::{AgentTool, ToolCallEventStream, ToolInput};
use agent_client_protocol::schema::v2 as acp_v2;
use anyhow::Result;
use gpui::{App, SharedString, Task};
use language_model::LanguageModelToolResultContent;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The single-select field key used when presenting a fixed list of options.
const CHOICE_FIELD: &str = "choice";
/// The free-text field key used when the user may type their own answer.
const OTHER_FIELD: &str = "other";

pub const QUESTIONNAIRE_META_KEY: &str = "zdsh.dev/questionnaire";

/// Ask focused questions when an answer is needed to continue the user's task.
/// Group related questions in one call. Provide stable question and option IDs,
/// short prompts, and distinct choices. Users can always write their own answer.
/// Do not ask for information already available in the conversation or tools.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AskUserToolInput {
    /// Questions to present together. Keep the group short and relevant to the current task.
    #[serde(default)]
    #[schemars(required)]
    pub questions: Vec<AskUserQuestion>,
    #[serde(default)]
    #[schemars(skip)]
    pub question: String,
    #[serde(default)]
    #[schemars(skip)]
    pub options: Vec<String>,
    #[serde(default = "default_allow_free_text")]
    #[schemars(skip)]
    pub allow_free_text: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AskUserQuestion {
    pub id: String,
    pub prompt: String,
    pub options: Vec<AskUserOption>,
    #[serde(default, alias = "allow_multiple")]
    pub allow_multiple: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
pub struct AskUserOption {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskUserAnswer {
    pub question_id: String,
    pub selected_option_ids: Vec<String>,
    pub freeform_text: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AskUserQuestionnaire {
    pub questions: Vec<AskUserQuestion>,
    pub legacy: bool,
    pub allow_other: bool,
}

impl AskUserQuestionnaire {
    pub fn from_input(input: &AskUserToolInput) -> Self {
        if input.questions.is_empty() {
            Self {
                questions: vec![AskUserQuestion {
                    id: "question".into(),
                    prompt: input.question.clone(),
                    options: input
                        .options
                        .iter()
                        .map(|label| AskUserOption {
                            id: label.clone(),
                            label: label.clone(),
                        })
                        .collect(),
                    allow_multiple: false,
                }],
                legacy: true,
                allow_other: input.allow_free_text,
            }
        } else {
            Self {
                questions: input.questions.clone(),
                legacy: false,
                allow_other: true,
            }
        }
    }

    pub fn choice_field(&self, index: usize) -> String {
        if self.legacy {
            CHOICE_FIELD.into()
        } else {
            format!("question_{index}")
        }
    }

    pub fn other_field(&self, index: usize) -> String {
        if self.legacy {
            OTHER_FIELD.into()
        } else {
            format!("other_{index}")
        }
    }

    pub fn from_schema(schema: &acp_v2::ElicitationSchema) -> Option<Self> {
        let metadata = schema.meta.as_ref()?.get(QUESTIONNAIRE_META_KEY)?;
        serde_json::from_value(metadata.clone()).ok()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AskUserToolOutput {
    QuestionsAnswered { answers: Vec<AskUserAnswer> },
    Answered { selected: String },
    Error { error: String },
}

impl From<AskUserToolOutput> for LanguageModelToolResultContent {
    fn from(value: AskUserToolOutput) -> Self {
        match value {
            AskUserToolOutput::QuestionsAnswered { answers } => {
                serde_json::json!({"answers": answers}).to_string().into()
            }
            AskUserToolOutput::Answered { selected } => {
                format!("The user selected: {selected}").into()
            }
            AskUserToolOutput::Error { error } => error.into(),
        }
    }
}

fn default_allow_free_text() -> bool {
    true
}

pub struct AskUserTool;

impl AgentTool for AskUserTool {
    type Input = AskUserToolInput;
    type Output = AskUserToolOutput;

    const NAME: &'static str = "ask_user";

    fn kind() -> acp_v2::ToolKind {
        acp_v2::ToolKind::Other
    }

    fn initial_title(
        &self,
        input: Result<Self::Input, serde_json::Value>,
        _cx: &mut App,
    ) -> SharedString {
        match input {
            Ok(input) if !input.questions.is_empty() => "Questions".into(),
            Ok(input) if !input.question.is_empty() => SharedString::from(input.question),
            _ => "Asking a question".into(),
        }
    }

    fn run(
        self: Arc<Self>,
        input: ToolInput<Self::Input>,
        event_stream: ToolCallEventStream,
        cx: &mut App,
    ) -> Task<Result<Self::Output, Self::Output>> {
        cx.spawn(async move |cx| {
            let input = input
                .recv()
                .await
                .map_err(|error| AskUserToolOutput::Error {
                    error: error.to_string(),
                })?;

            let questionnaire = AskUserQuestionnaire::from_input(&input);
            let mut question_ids = std::collections::HashSet::new();
            for question in &questionnaire.questions {
                let mut option_ids = std::collections::HashSet::new();
                if question.id.trim().is_empty() || question.prompt.trim().is_empty()
                    || !question_ids.insert(&question.id)
                    || question.options.iter().any(|option| option.id.trim().is_empty() || option.label.trim().is_empty() || !option_ids.insert(&option.id))
                {
                    return Err(AskUserToolOutput::Error { error: "Provide nonempty prompts and labels, and unique question and option IDs.".into() });
                }
                if !questionnaire.allow_other && question.options.len() < 2 {
                    return Err(AskUserToolOutput::Error { error: "The ask_user tool needs at least two options, or allow_free_text set to true.".into() });
                }
            }
            let mut schema = if questionnaire.legacy {
                build_schema(&input.options, input.allow_free_text)
            } else {
                let mut schema = acp_v2::ElicitationSchema::new();
                for (index, question) in questionnaire.questions.iter().enumerate() {
                    if !question.options.is_empty() {
                        let options = question.options.iter().map(|option| acp_v2::EnumOption::new(option.id.clone(), option.label.clone())).collect::<Vec<_>>();
                        schema = if question.allow_multiple {
                            schema.property(questionnaire.choice_field(index), acp_v2::MultiSelectPropertySchema::titled(options).title(question.prompt.clone()), false)
                        } else {
                            schema.property(questionnaire.choice_field(index), acp_v2::StringPropertySchema::new().title(question.prompt.clone()).one_of(options), false)
                        };
                    }
                    schema = schema.property(questionnaire.other_field(index), acp_v2::StringPropertySchema::new().title("Other"), false);
                }
                schema
            };
            let metadata = serde_json::to_value(&questionnaire).map_err(|error| AskUserToolOutput::Error { error: error.to_string() })?;
            schema.meta = Some(acp_v2::Meta::from_iter([(QUESTIONNAIRE_META_KEY.into(), metadata)]));
            let message = if questionnaire.legacy { input.question.clone() } else { "Questions".into() };
            let prompt = cx.update(|cx| event_stream.request_elicitation(message, schema, cx));
            let response = prompt.await.map_err(|error| AskUserToolOutput::Error { error: error.to_string() })?;

            if !questionnaire.legacy {
                let acp_v2::ElicitationAction::Accept(accept) = response.action else {
                    return Err(AskUserToolOutput::Error { error: "The user skipped the questions. Continue without assuming any answer or approval.".into() });
                };
                let content = accept.content.unwrap_or_default();
                let mut answers = Vec::new();
                for (index, question) in questionnaire.questions.iter().enumerate() {
                    let selected_option_ids = match content.get(&questionnaire.choice_field(index)) {
                        Some(acp_v2::ElicitationContentValue::String(value)) => vec![value.clone()],
                        Some(acp_v2::ElicitationContentValue::StringArray(values)) => values.clone(),
                        _ => Vec::new(),
                    };
                    let freeform_text = string_field(&content, &questionnaire.other_field(index));
                    if (selected_option_ids.is_empty() && freeform_text.is_none())
                        || (!question.allow_multiple && selected_option_ids.len() > 1)
                        || selected_option_ids.iter().any(|id| !question.options.iter().any(|option| &option.id == id))
                    {
                        return Err(AskUserToolOutput::Error { error: format!("No valid answer was supplied for question {}", question.id) });
                    }
                    answers.push(AskUserAnswer { question_id: question.id.clone(), selected_option_ids, freeform_text });
                }
                event_stream.update_fields(|update| update.title("Answered questions"));
                return Ok(AskUserToolOutput::QuestionsAnswered { answers });
            }

            let selected = match response.action {
                acp_v2::ElicitationAction::Accept(accept) => {
                    let content = accept.content.unwrap_or_default();
                    // A typed answer takes precedence over a picked option.
                    string_field(&content, OTHER_FIELD)
                        .or_else(|| string_field(&content, CHOICE_FIELD))
                        .ok_or_else(|| AskUserToolOutput::Error {
                            error: "The user submitted the form without providing an answer."
                                .to_string(),
                        })?
                }
                acp_v2::ElicitationAction::Decline => {
                    return Err(AskUserToolOutput::Error {
                        error: "The user declined to answer the question.".to_string(),
                    });
                }
                acp_v2::ElicitationAction::Cancel => {
                    return Err(AskUserToolOutput::Error {
                        error: "The user cancelled the question without answering.".to_string(),
                    });
                }
                _ => {
                    return Err(AskUserToolOutput::Error {
                        error: "The question was dismissed without an answer.".to_string(),
                    });
                }
            };

            event_stream.update_fields(|update| update.title(format!("Answered: {selected}")));

            Ok(AskUserToolOutput::Answered { selected })
        })
    }
}

/// Builds the elicitation form schema for a question.
///
/// - Presenting a fixed list of options adds a required single-select field,
///   unless free text is also allowed (in which case picking is optional).
/// - Allowing free text adds a text field, required only when there are no
///   options to choose from.
fn build_schema(options: &[String], allow_free_text: bool) -> acp_v2::ElicitationSchema {
    let mut schema = acp_v2::ElicitationSchema::new();

    if !options.is_empty() {
        let enum_options = options
            .iter()
            .map(|label| acp_v2::EnumOption::new(label.clone(), label.clone()))
            .collect::<Vec<_>>();
        schema = schema.property(
            CHOICE_FIELD,
            acp_v2::StringPropertySchema::new()
                .title("Choose an option")
                .one_of(enum_options),
            !allow_free_text,
        );
    }

    if allow_free_text {
        let title = if options.is_empty() {
            "Your answer"
        } else {
            "Or type your own answer"
        };
        schema = schema.property(
            OTHER_FIELD,
            acp_v2::StringPropertySchema::new().title(title),
            options.is_empty(),
        );
    }

    schema
}

/// Extracts a non-empty string value for `key` from an elicitation response.
fn string_field(
    content: &BTreeMap<String, acp_v2::ElicitationContentValue>,
    key: &str,
) -> Option<String> {
    match content.get(key) {
        Some(acp_v2::ElicitationContentValue::String(value)) if !value.trim().is_empty() => {
            Some(value.clone())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    fn accept_with(
        entries: impl IntoIterator<Item = (&'static str, &'static str)>,
    ) -> acp_v2::CreateElicitationResponse {
        let content: BTreeMap<String, acp_v2::ElicitationContentValue> = entries
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string(),
                    acp_v2::ElicitationContentValue::String(value.to_string()),
                )
            })
            .collect();
        acp_v2::CreateElicitationResponse::new(acp_v2::ElicitationAction::Accept(
            acp_v2::ElicitationAcceptAction::new().content(content),
        ))
    }

    #[gpui::test]
    async fn test_ask_user_returns_selected_option(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool_call_id = event_stream.tool_call_id().clone();
        let tool = Arc::new(AskUserTool);
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    questions: Vec::new(),
                    question: "Which approach?".into(),
                    options: vec!["Approach A".into(), "Approach B".into()],
                    allow_free_text: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert_eq!(request.tool_call_id.0, tool_call_id.0);
        assert!(Arc::ptr_eq(&request.tool_call_id.0, &tool_call_id.0));
        assert_eq!(request.message, "Which approach?");
        assert!(request.schema.properties.contains_key(CHOICE_FIELD));
        assert!(!request.schema.properties.contains_key(OTHER_FIELD));

        request
            .response
            .send(accept_with([(CHOICE_FIELD, "Approach B")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "Approach B"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_free_text_answer(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool);
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    questions: Vec::new(),
                    question: "What should I name it?".into(),
                    options: vec![],
                    allow_free_text: true,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert!(request.schema.properties.contains_key(OTHER_FIELD));
        assert!(!request.schema.properties.contains_key(CHOICE_FIELD));

        request
            .response
            .send(accept_with([(OTHER_FIELD, "widget")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "widget"),
            other => panic!("expected an answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_free_text_takes_precedence(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool);
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    questions: Vec::new(),
                    question: "Pick or type".into(),
                    options: vec!["A".into(), "B".into()],
                    allow_free_text: true,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        assert!(request.schema.properties.contains_key(CHOICE_FIELD));
        assert!(request.schema.properties.contains_key(OTHER_FIELD));

        request
            .response
            .send(accept_with([(CHOICE_FIELD, "A"), (OTHER_FIELD, "custom")]))
            .unwrap();

        match task.await {
            Ok(AskUserToolOutput::Answered { selected }) => assert_eq!(selected, "custom"),
            other => panic!("expected the typed answer, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_decline_is_an_error(cx: &mut TestAppContext) {
        let (event_stream, mut event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool);
        let task = cx.update(|cx| {
            tool.run(
                ToolInput::resolved(AskUserToolInput {
                    questions: Vec::new(),
                    question: "Proceed?".into(),
                    options: vec!["Yes".into(), "No".into()],
                    allow_free_text: false,
                }),
                event_stream,
                cx,
            )
        });

        let request = event_rx.expect_elicitation().await;
        request
            .response
            .send(acp_v2::CreateElicitationResponse::new(
                acp_v2::ElicitationAction::Decline,
            ))
            .unwrap();

        match task.await {
            Err(AskUserToolOutput::Error { error }) => {
                assert!(error.contains("declined"), "got: {error}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }

    #[gpui::test]
    async fn test_ask_user_requires_options_or_free_text(cx: &mut TestAppContext) {
        let (event_stream, _event_rx) = ToolCallEventStream::test();
        let tool = Arc::new(AskUserTool);
        let result = cx
            .update(|cx| {
                tool.run(
                    ToolInput::resolved(AskUserToolInput {
                        questions: Vec::new(),
                        question: "Proceed?".into(),
                        options: vec!["Only one".into()],
                        allow_free_text: false,
                    }),
                    event_stream,
                    cx,
                )
            })
            .await;

        match result {
            Err(AskUserToolOutput::Error { error }) => {
                assert!(error.contains("at least two"), "got: {error}");
            }
            other => panic!("expected an error, got {other:?}"),
        }
    }
}
