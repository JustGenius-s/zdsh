use crate::metadata::{
    CommandBuilder, CommandSignatureGenerators, Generator, GeneratorResults,
    GeneratorResultsCollector, Importance, Order, Priority, Suggestion,
};
use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashSet;

lazy_static! {
    /// Regex to parse Makefiles for targets with either leading or inline comments that could be
    /// used as descriptions. The regex can be parsed as follows:
    /// `((?:^#.*\n)*)`: Capture group to match any leading comments (comments on the line before a
    ///   target).
    /// `(?:^\.[A-Z_]+:.*\n)`: non-capture group to avoid matching on some upper cased special
    ///   targets.
    /// `(^\S*?)`: Capture group to capture the target name.
    /// `(?:\s#+[ \t]*(.+))`: Match any trailing comments (while only capturing the name of the
    ///   comment) of the target. Such as `target: {command} # {trailing comment}`
    static ref MAKE_TARGET_RE: Regex =
        Regex::new(r"(?m)((?:^#.*\n)*)(?:^\.[A-Z_]+:.*\n)*(^\S*?):.*?(?:\s#+[ \t]*(.+))?$")
            .unwrap();
    static ref STARTS_WITH_COMMENT: Regex = Regex::new(r"^#+\s*").unwrap();
    static ref SPECIAL_TARGETS: HashSet<&'static str> = HashSet::from_iter([
        ".PHONY",
        ".SUFFIXES",
        ".DEFAULT",
        ".PRECIOUS",
        ".INTERMEDIATE",
        ".SECONDARY",
        ".SECONDEXPANSION",
        ".DELETE_ON_ERROR",
        ".IGNORE",
        ".LOW_RESOLUTION_TIME",
        ".SILENT",
        ".EXPORT_ALL_VARIABLES",
        ".NOTPARALLEL",
        ".ONESHELL",
        ".POSIX",
    ]);
}

fn list_targets_post_process(output: &str) -> GeneratorResults {
    MAKE_TARGET_RE
        .captures_iter(output)
        .filter_map(|capture| {
            let target = capture.get(2)?.as_str();
            if SPECIAL_TARGETS.contains(target) {
                return None;
            }

            let leading_comment = capture
                .get(1)
                .map(|capture_match| capture_match.as_str().trim());

            // The regex may not have a match for the capture group matching inline_comment. Both
            // target and leading comment should always have a match.
            let inline_comment = capture
                .get(3)
                .map(|capture_match| capture_match.as_str().trim());

            // Determine what the description should be based on the present of either a leading
            // comment or inline comment on the target. If neither exist, fallback to "Make target"
            // as the description
            let description = match (inline_comment, leading_comment) {
                (Some(inline_comment), _) if !inline_comment.is_empty() => inline_comment.into(),
                (_, Some(leading_comment)) if !leading_comment.is_empty() => {
                    // Remove the first part of the match so that we have the comment without any
                    // leading `#`s.
                    STARTS_WITH_COMMENT.replace(leading_comment, "")
                }
                // If there are no comments, fallback to `Make target` as the description.
                _ => "Make target".into(),
            };

            Some(
                Suggestion::with_description(target.trim(), description.trim())
                    .with_priority(Priority::Global(Importance::More(Order(80)))),
            )
        })
        .collect_unordered_results()
}

pub fn generator() -> CommandSignatureGenerators {
    CommandSignatureGenerators::new("make").add_generator(
        "list_targets",
        Generator::script(
            CommandBuilder::single_command("cat [Mm]akefile"),
            list_targets_post_process,
        ),
    )
}
