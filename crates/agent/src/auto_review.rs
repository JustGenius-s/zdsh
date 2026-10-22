//! Has a model review a thread's request to write to specific paths outside
//! its sandbox, so the user is only asked when the review doesn't allow it.
//!
//! The reviewer sees only what the user wrote and the request itself. The
//! agent's own text, tool results, fetched pages and project rules could all
//! carry instructions planted for it, so none of them are sent.

use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context as _, Result};
use futures::{FutureExt as _, StreamExt as _};
use gpui::AsyncApp;
use language_model::{
    CompletionIntent, LanguageModel, LanguageModelCompletionEvent, LanguageModelRegistry,
    LanguageModelRequest, LanguageModelRequestMessage, Role,
};
use serde::{Deserialize, Serialize};

use crate::templates::{Template, Templates};

/// How long a review may take before the user is asked instead.
pub(crate) const AUTO_REVIEW_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) struct AutoReviewInput {
    pub model: LanguageModel,
    pub templates: Arc<Templates>,
    pub worktrees: Vec<PathBuf>,
    /// The text of the user's messages, oldest first. For a subagent, these
    /// come from the thread the user talks to, since its own prompts were
    /// written by the agent that started it.
    pub user_messages: Vec<String>,
    pub request: ReviewedRequest,
}

/// Everything here was written by the agent, and the reviewer is told so.
#[derive(Serialize)]
pub(crate) struct ReviewedRequest {
    pub tool: String,
    pub input: serde_json::Value,
    pub write_paths: Vec<PathBuf>,
    pub agent_justification: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoReviewOutcome {
    Allowed {
        reason: String,
    },
    /// The review didn't allow the request, so the user decides. The reason
    /// is the review's own, or why the review failed.
    NotAllowed {
        reason: String,
    },
}

#[derive(Serialize)]
struct AutoReviewTemplate {
    worktrees: Vec<String>,
    platform: &'static str,
}

impl Template for AutoReviewTemplate {
    const TEMPLATE_NAME: &'static str = "auto_review.hbs";
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Verdict {
    risk: Risk,
    decision: Decision,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Risk {
    Low,
    Medium,
    High,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Allow,
    Deny,
}

pub(crate) async fn review(input: AutoReviewInput, cx: &AsyncApp) -> AutoReviewOutcome {
    let timeout = cx.background_executor().timer(AUTO_REVIEW_TIMEOUT);
    let reply = futures::select_biased! {
        reply = request_verdict(&input, cx).fuse() => reply,
        _ = timeout.fuse() => {
            return AutoReviewOutcome::NotAllowed {
                reason: "The auto review timed out.".into(),
            };
        }
    };
    match reply {
        Ok(reply) => parse_verdict(&reply),
        Err(error) => AutoReviewOutcome::NotAllowed {
            reason: format!("The auto review failed: {error:#}"),
        },
    }
}

async fn request_verdict(input: &AutoReviewInput, cx: &AsyncApp) -> Result<String> {
    let request = build_request(input)?;
    let provider =
        cx.update(|cx| LanguageModelRegistry::read_global(cx).provider_for_model(&input.model))?;
    let mut events = provider
        .stream_completion(&input.model, request, cx)
        .await?;
    let mut reply = String::new();
    while let Some(event) = events.next().await {
        if let LanguageModelCompletionEvent::Text(text) = event? {
            reply.push_str(&text);
        }
    }
    Ok(reply)
}

pub(crate) fn build_request(input: &AutoReviewInput) -> Result<LanguageModelRequest> {
    let instructions = AutoReviewTemplate {
        worktrees: input
            .worktrees
            .iter()
            .map(|worktree| worktree.display().to_string())
            .collect(),
        platform: std::env::consts::OS,
    }
    .render(&input.templates)
    .context("failed to render the auto review instructions")?;
    // JSON keeps text from the agent from passing itself off as part of the
    // surrounding message.
    let user_messages = serde_json::to_string_pretty(&input.user_messages)?;
    let request = serde_json::to_string_pretty(&input.request)?;
    let message = format!(
        "The user's messages in this thread, oldest first:\n\n{user_messages}\n\n\
         The request to review, written by the agent:\n\n{request}"
    );

    Ok(LanguageModelRequest {
        intent: Some(CompletionIntent::AutoReview),
        messages: vec![
            LanguageModelRequestMessage {
                role: Role::System,
                content: vec![instructions.into()],
                cache: false,
                reasoning_details: None,
            },
            LanguageModelRequestMessage {
                role: Role::User,
                content: vec![message.into()],
                cache: false,
                reasoning_details: None,
            },
        ],
        ..Default::default()
    })
}

/// Only low risk allowed, medium risk either way, and high risk denied are
/// valid verdicts. Anything else counts as a failed review.
pub(crate) fn parse_verdict(reply: &str) -> AutoReviewOutcome {
    let verdict = match serde_json::from_str::<Verdict>(reply.trim()) {
        Ok(verdict) => verdict,
        Err(error) => {
            return AutoReviewOutcome::NotAllowed {
                reason: format!("The auto review's reply wasn't valid: {error}"),
            };
        }
    };
    let reason = verdict.reason.filter(|reason| !reason.trim().is_empty());
    match (verdict.risk, verdict.decision) {
        (Risk::Low | Risk::Medium, Decision::Allow) => AutoReviewOutcome::Allowed {
            reason: reason.unwrap_or_else(|| "The auto review allowed this request.".into()),
        },
        (Risk::Medium | Risk::High, Decision::Deny) => AutoReviewOutcome::NotAllowed {
            reason: reason.unwrap_or_else(|| "The auto review didn't allow this request.".into()),
        },
        (Risk::Low, Decision::Deny) | (Risk::High, Decision::Allow) => {
            AutoReviewOutcome::NotAllowed {
                reason: "The auto review's verdict contradicted its risk rating.".into(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(reason: &str) -> AutoReviewOutcome {
        AutoReviewOutcome::Allowed {
            reason: reason.into(),
        }
    }

    fn not_allowed(reason: &str) -> AutoReviewOutcome {
        AutoReviewOutcome::NotAllowed {
            reason: reason.into(),
        }
    }

    #[test]
    fn verdicts_allow_only_valid_combinations() {
        for (reply, outcome) in [
            (
                r#"{"risk": "low", "decision": "allow", "reason": "Build output."}"#,
                allowed("Build output."),
            ),
            (
                r#" {"risk": "medium", "decision": "allow", "reason": "The user asked."} "#,
                allowed("The user asked."),
            ),
            (
                r#"{"risk": "medium", "decision": "allow"}"#,
                allowed("The auto review allowed this request."),
            ),
            (
                r#"{"risk": "medium", "decision": "deny", "reason": "Not requested."}"#,
                not_allowed("Not requested."),
            ),
            (
                r#"{"risk": "high", "decision": "deny", "reason": "Sends secrets."}"#,
                not_allowed("Sends secrets."),
            ),
            (
                r#"{"risk": "high", "decision": "allow", "reason": "Fine."}"#,
                not_allowed("The auto review's verdict contradicted its risk rating."),
            ),
            (
                r#"{"risk": "low", "decision": "deny", "reason": "No."}"#,
                not_allowed("The auto review's verdict contradicted its risk rating."),
            ),
        ] {
            assert_eq!(parse_verdict(reply), outcome, "{reply}");
        }
    }

    #[test]
    fn replies_that_arent_exactly_a_verdict_fail() {
        for reply in [
            "",
            "Allow it.",
            "```json\n{\"risk\": \"low\", \"decision\": \"allow\"}\n```",
            r#"{"risk": "low", "decision": "allow"} Done."#,
            r#"{"risk": "none", "decision": "allow"}"#,
            r#"{"risk": "low", "decision": "allow", "confidence": 1}"#,
        ] {
            assert!(
                matches!(parse_verdict(reply), AutoReviewOutcome::NotAllowed { .. }),
                "{reply:?}"
            );
        }
    }
}
