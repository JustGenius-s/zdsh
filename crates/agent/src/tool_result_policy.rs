//! Adjusts tool results before the model sees them. The user still sees each
//! tool's complete output, which travels separately as the raw output.

use language_model::{
    LanguageModelToolResult, LanguageModelToolResultContent, LanguageModelToolUse,
    LanguageModelToolUseInput,
};
use std::sync::Arc;

/// The most text, in bytes, of a single tool result sent to the model. About
/// 12,500 tokens; tools with their own output limits stay well below it.
const MAX_TOOL_RESULT_TEXT_BYTES: usize = 50_000;

/// Numbers of consecutive identical calls at which the model is reminded that
/// it is repeating itself.
const REPEAT_REMINDER_THRESHOLDS: [usize; 3] = [3, 5, 8];

/// The reminder at the first threshold, before repeats are treated as stuck.
pub(crate) const FIRST_REPEAT_REMINDER: &str = "You are repeating the exact same tool call with \
    identical arguments. Carefully analyze the previous result before calling again: if the task \
    is not complete, try a different approach or different arguments instead of repeating the \
    call.";

/// Longest preview of a repeated call's arguments in a reminder.
const REPEAT_REMINDER_ARGUMENTS_PREVIEW_CHARS: usize = 500;

/// Applies every policy to `tool_result`. `call` is the call that produced it,
/// if it can still be found, with the number of identical calls made in a row
/// up to and including it since the last user message.
pub(crate) fn apply(
    tool_result: &mut LanguageModelToolResult,
    call: Option<(&LanguageModelToolUse, usize)>,
) {
    truncate_oversized_text(&mut tool_result.content, MAX_TOOL_RESULT_TEXT_BYTES);
    if let Some((tool_use, consecutive_calls)) = call
        && let Some(reminder) = repeat_reminder(tool_use, consecutive_calls)
    {
        tool_result
            .content
            .push(LanguageModelToolResultContent::Text(reminder.into()));
    }
}

/// Counts `call` and the model responses directly before it that made only
/// that same call. `earlier_responses` holds each earlier response's calls,
/// newest response first. Calls made side by side in one response are not
/// counted against each other, since none of them saw another's result.
pub(crate) fn consecutive_identical_calls<'a>(
    call: &LanguageModelToolUse,
    earlier_responses: impl Iterator<Item = Vec<&'a LanguageModelToolUse>>,
) -> usize {
    1 + earlier_responses
        .take_while(|calls| {
            !calls.is_empty()
                && calls
                    .iter()
                    .all(|earlier| earlier.name == call.name && earlier.input == call.input)
        })
        .count()
}

/// Keeps the start and end of text that exceeds `max_bytes`, so a single
/// oversized result cannot fill the context window. Images are left as they are.
fn truncate_oversized_text(content: &mut Vec<LanguageModelToolResultContent>, max_bytes: usize) {
    let text_bytes: usize = content
        .iter()
        .map(|part| match part {
            LanguageModelToolResultContent::Text(text) => text.len(),
            LanguageModelToolResultContent::Image(_) => 0,
        })
        .sum();
    if text_bytes <= max_bytes {
        return;
    }

    let mut text = String::with_capacity(text_bytes);
    let mut first_text_ix = None;
    for (ix, part) in content.iter().enumerate() {
        if let LanguageModelToolResultContent::Text(part) = part {
            first_text_ix.get_or_insert(ix);
            text.push_str(part);
        }
    }
    let Some(first_text_ix) = first_text_ix else {
        return;
    };

    let head_end = text.floor_char_boundary(max_bytes / 2);
    let tail_start = text.ceil_char_boundary(text.len() - max_bytes / 2);
    let omitted_bytes = tail_start - head_end;
    let truncated: Arc<str> = format!(
        "{}\n\n[{omitted_bytes} bytes were omitted from the middle of this tool result to save \
         context; the user can still see all of it. If you need the omitted part, call the tool \
         again with narrower arguments, such as a smaller line range or a more specific \
         pattern.]\n\n{}",
        &text[..head_end],
        &text[tail_start..],
    )
    .into();

    let mut ix = 0;
    content.retain(|part| {
        let keep = ix == first_text_ix || !matches!(part, LanguageModelToolResultContent::Text(_));
        ix += 1;
        keep
    });
    if let Some(part) = content
        .iter_mut()
        .find(|part| matches!(part, LanguageModelToolResultContent::Text(_)))
    {
        *part = LanguageModelToolResultContent::Text(truncated);
    }
}

fn repeat_reminder(tool_use: &LanguageModelToolUse, consecutive_calls: usize) -> Option<String> {
    if !REPEAT_REMINDER_THRESHOLDS.contains(&consecutive_calls) {
        return None;
    }
    if Some(&consecutive_calls) == REPEAT_REMINDER_THRESHOLDS.first() {
        return Some(FIRST_REPEAT_REMINDER.to_string());
    }
    let arguments = match &tool_use.input {
        LanguageModelToolUseInput::Json(input) => input.to_string(),
        LanguageModelToolUseInput::Text(input) => input.clone(),
    };
    let preview = if arguments.chars().count() > REPEAT_REMINDER_ARGUMENTS_PREVIEW_CHARS {
        let preview: String = arguments
            .chars()
            .take(REPEAT_REMINDER_ARGUMENTS_PREVIEW_CHARS)
            .collect();
        format!("{preview}…")
    } else {
        arguments
    };
    Some(format!(
        "Repeated tool call detected:\n\
         - tool: {}\n\
         - consecutive_calls: {consecutive_calls}\n\
         - arguments: {preview}\n\
         The repeated calls are not making progress. Do not call this tool with these exact \
         arguments again. Inspect the latest result and choose a different action, different \
         arguments, or finish the task if enough evidence has been gathered.",
        tool_use.name,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool_use(id: &str, name: &str, input: serde_json::Value) -> LanguageModelToolUse {
        LanguageModelToolUse {
            id: id.into(),
            name: name.into(),
            raw_input: input.to_string(),
            input: LanguageModelToolUseInput::Json(input),
            is_input_complete: true,
            thought_signature: None,
        }
    }

    fn texts(content: &[LanguageModelToolResultContent]) -> Vec<String> {
        content
            .iter()
            .filter_map(|part| match part {
                LanguageModelToolResultContent::Text(text) => Some(text.to_string()),
                LanguageModelToolResultContent::Image(_) => None,
            })
            .collect()
    }

    #[test]
    fn test_short_text_is_unchanged() {
        let mut content = vec![LanguageModelToolResultContent::Text("hello".into())];
        truncate_oversized_text(&mut content, 10);
        assert_eq!(texts(&content), vec!["hello"]);
    }

    #[test]
    fn test_oversized_text_keeps_head_and_tail() {
        let mut content = vec![
            LanguageModelToolResultContent::Text("aaaaaaaaaa".into()),
            LanguageModelToolResultContent::Text("bbbbbbbbbb".into()),
        ];
        truncate_oversized_text(&mut content, 8);
        let texts = texts(&content);
        assert_eq!(texts.len(), 1);
        assert!(texts[0].starts_with("aaaa\n\n[12 bytes were omitted"));
        assert!(texts[0].ends_with("]\n\nbbbb"));
    }

    #[test]
    fn test_truncation_respects_char_boundaries() {
        let mut content = vec![LanguageModelToolResultContent::Text("éééééé".into())];
        truncate_oversized_text(&mut content, 5);
        let texts = texts(&content);
        assert!(texts[0].starts_with("é\n\n[8 bytes were omitted"));
        assert!(texts[0].ends_with("]\n\né"));
    }

    #[test]
    fn test_consecutive_identical_calls() {
        let latest = tool_use("4", "grep", json!({"regex": "a", "path": "src"}));
        let same_reordered = tool_use("3", "grep", json!({"path": "src", "regex": "a"}));
        let same = tool_use("2", "grep", json!({"regex": "a", "path": "src"}));
        let different = tool_use("1", "grep", json!({"regex": "b"}));
        let earlier_same = tool_use("0", "grep", json!({"regex": "a", "path": "src"}));
        assert_eq!(
            consecutive_identical_calls(
                &latest,
                [
                    vec![&same_reordered, &same],
                    vec![&same, &different],
                    vec![&earlier_same],
                ]
                .into_iter()
            ),
            2
        );
        assert_eq!(consecutive_identical_calls(&latest, std::iter::empty()), 1);
        assert_eq!(
            consecutive_identical_calls(&latest, [Vec::new(), vec![&same]].into_iter()),
            1
        );
    }

    #[test]
    fn test_reminders_fire_at_thresholds() {
        let call = tool_use("1", "grep", json!({"regex": "a"}));
        for count in 1..=9 {
            let mut result = LanguageModelToolResult {
                tool_use_id: call.id.clone(),
                tool_name: call.name.clone(),
                is_error: false,
                content: vec![LanguageModelToolResultContent::Text("found".into())],
                output: None,
            };
            apply(&mut result, Some((&call, count)));
            let texts = texts(&result.content);
            match count {
                3 => assert!(texts[1].starts_with("You are repeating")),
                5 | 8 => assert!(texts[1].contains(&format!("consecutive_calls: {count}"))),
                _ => assert_eq!(texts, vec!["found"]),
            }
        }
    }
}
