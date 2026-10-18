use super::SessionState;
use agent_client_protocol::schema::{v1 as acp_v1, v2 as acp_v2};
use anyhow::{Context as _, Result};
use dsh_agent::{AgentError, ModelChunk, SessionEvent};
use gpui::AsyncApp;

pub(super) fn apply_event(
    session: &SessionState,
    visible_turn: &mut Option<dsh_agent::TurnId>,
    event: SessionEvent,
    cx: &mut AsyncApp,
) -> Result<()> {
    match &event {
        SessionEvent::TurnStarted { turn, .. } => {
            *visible_turn = Some(*turn);
        }
        SessionEvent::ModelOutput { turn, .. } | SessionEvent::ToolFinished { turn, .. }
            if Some(*turn) != *visible_turn =>
        {
            return Ok(());
        }
        _ => {}
    }
    if session.closed.get() {
        return Ok(());
    }
    match event {
        SessionEvent::ModelOutput {
            turn,
            step,
            chunk: ModelChunk::ToolCall(call),
        } => {
            let id = crate::extension_tools::call_id(turn, step, &call.id);
            session.thread.update(cx, |thread, cx| {
                if thread
                    .tool_call(&acp_v2::ToolCallId::new(id.clone()))
                    .is_none()
                {
                    thread.upsert_tool_call(
                        acp_v1::ToolCall::new(id, call.name)
                            .raw_input(call.arguments)
                            .status(acp_v1::ToolCallStatus::Pending),
                        cx,
                    )?;
                }
                Ok(())
            })?
        }
        SessionEvent::ToolFinished {
            turn,
            step,
            call_id,
            result,
            ..
        } => {
            let id = crate::extension_tools::call_id(turn, step, &call_id);
            let status = if result.is_ok() {
                acp_v1::ToolCallStatus::Completed
            } else {
                acp_v1::ToolCallStatus::Failed
            };
            let output = match result {
                Ok(value) => value,
                Err(error) => serde_json::json!({"error": error.to_string()}),
            };
            session.thread.update(cx, |thread, cx| {
                thread.update_tool_call(
                    acp_v1::ToolCallUpdate::new(
                        id,
                        acp_v1::ToolCallUpdateFields::new()
                            .status(status)
                            .raw_output(output),
                    ),
                    cx,
                )
            })?
        }
        SessionEvent::ModelOutput {
            chunk: ModelChunk::Text(text),
            ..
        } => push_text(session, text, false, cx),
        SessionEvent::ModelOutput {
            chunk: ModelChunk::Thinking { text, .. },
            ..
        } => push_text(session, text, true, cx),
        SessionEvent::ModelOutput {
            chunk: ModelChunk::Usage(usage),
            ..
        } => {
            let selected = session.active_model.borrow();
            let model = selected
                .as_ref()
                .context("DSH model selection is missing")?;
            let input_tokens = usage
                .input_tokens
                .saturating_add(usage.cache_read_input_tokens)
                .saturating_add(usage.cache_creation_input_tokens);
            let usage = acp_thread::TokenUsage {
                max_tokens: model.max_token_count,
                used_tokens: input_tokens.saturating_add(usage.output_tokens),
                input_tokens,
                output_tokens: usage.output_tokens,
                max_output_tokens: model.max_output_tokens,
            };
            session
                .thread
                .update(cx, |thread, cx| thread.update_token_usage(Some(usage), cx))
        }
        _ => Ok(()),
    }
}

fn push_text(session: &SessionState, text: String, thought: bool, cx: &mut AsyncApp) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    session.thread.update(cx, |thread, cx| {
        thread.push_assistant_content_block(
            acp_v2::ContentBlock::Text(acp_v2::TextContent::new(text)),
            thought,
            cx,
        )
    })
}

pub(super) fn prompt_response(
    result: std::result::Result<String, AgentError>,
) -> Result<acp_v1::PromptResponse> {
    let reason = match result {
        Ok(_) => acp_v1::StopReason::EndTurn,
        Err(AgentError::Cancelled | AgentError::Stopped) => acp_v1::StopReason::Cancelled,
        Err(AgentError::OutputLimit) => acp_v1::StopReason::MaxTokens,
        Err(AgentError::Refused) => acp_v1::StopReason::Refusal,
        Err(error) => return Err(error.into()),
    };
    Ok(acp_v1::PromptResponse::new(reason))
}
