use crate::{
    completer::{self, CompleterOptions, CompletionContext, Match, MatchStrategy},
    parsers::simple::all_parsed_commands,
};
use anyhow::{Result, bail};
use std::{cmp::Reverse, collections::HashMap, ops::Range};

#[derive(Clone, Debug)]
pub struct CommandCompletion {
    pub label: String,
    pub replacement: String,
    pub description: Option<String>,
    pub range: Range<usize>,
}

/// Completes a UTF-8 cursor position using host-provided paths, environment, and shell queries.
/// Returned ranges cover the entire current token, including any text after the cursor.
pub async fn complete<T: CompletionContext>(
    text: &str,
    cursor: usize,
    environment: Option<&HashMap<String, String>>,
    context: &T,
) -> Result<Vec<CommandCompletion>> {
    if !text.is_char_boundary(cursor) {
        bail!("The completion cursor is not on a character boundary.");
    }
    let results = completer::suggestions(
        text,
        cursor,
        environment,
        CompleterOptions::default(),
        context,
    )
    .await;
    let Some(results) = results else {
        return Ok(Vec::new());
    };
    let mut range: Range<usize> = results.replacement_span.into();
    if text.get(range.clone()).is_none() {
        bail!("Warp returned an invalid completion range.");
    }
    if let Some(token) = all_parsed_commands(&text[range.start..], context.escape_char())
        .next()
        .and_then(|command| command.parts.into_iter().next())
        && token.span.start() == 0
        && token.span.end() >= range.len()
    {
        range.end = range.start + token.span.end();
    }
    // Warp already matched parsed tokens. Filtering raw quoted or escaped text again
    // would drop valid file candidates, so retain those matches and their fuzzy scores.
    let mut suggestions = results.suggestions;
    suggestions.retain(|item| {
        !item.suggestion.is_hidden || matches!(item.match_type, Match::Exact { .. })
    });
    suggestions.sort_by_key(|item| match &item.match_type {
        Match::Exact {
            is_case_sensitive: true,
        } => (0, Reverse(0)),
        Match::Exact { .. } => (1, Reverse(0)),
        Match::Prefix { .. } => (2, Reverse(0)),
        Match::Fuzzy { match_result } => (3, Reverse(match_result.score)),
    });
    Ok(suggestions
        .into_iter()
        .map(|matched| {
            let suggestion = matched.suggestion;
            CommandCompletion {
                label: suggestion.display.to_string(),
                replacement: suggestion.replacement.to_string(),
                description: suggestion.description,
                range: range.clone(),
            }
        })
        .collect())
}

/// Picks the newest longer prefix match without adding the suggested suffix to the buffer.
pub fn history_suggestion<'a>(
    text: &str,
    history: impl DoubleEndedIterator<Item = &'a str>,
) -> Option<String> {
    if text.trim().is_empty() {
        return None;
    }
    history
        .rev()
        .find(|command| {
            command.len() > text.len()
                && MatchStrategy::CaseSensitive
                    .get_match_type(text, command)
                    .is_some()
        })
        .map(str::to_owned)
}
