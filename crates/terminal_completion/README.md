# Terminal completion

This crate owns command tokenization, shell escaping, embedded command signatures, prefix/fuzzy matching, ranking, token replacement ranges, and history prefix suggestions. It has no GPUI, WarpUI, font, application-state, CLI initialization, process-launching, or executor dependency. `terminal_core` provides shared shell and text primitives.

`complete(text, cursor, environment, context)` returns labels, replacement text, descriptions, and UTF-8 byte ranges. The range includes the remainder of a quoted or escaped token after the cursor. `history_suggestion` accepts a double-ended history iterator and chooses the newest longer prefix match without editing input.

## Host interfaces

`CompletionContext` supplies executable names, the command registry, optional aliases/functions/builtins and environment variable names, shell family, and optional path and generator contexts. The request's environment map supplies variable values. `PathCompletionContext` supplies asynchronous directory listings, current/home directories, CDPATH, and path separators. `GeneratorContext` represents an explicit host-owned shell query; it owns execution and returns stdout, stderr, and exit status. The engine never launches a process by itself.

The Zed adapter in `terminal/src/completions.rs` supplies local directories, launch-environment PATH and common Bash/Zsh builtins. It does not provide a generator context, so dynamic shell queries are disabled. Native shell completions, runtime exports, custom aliases/functions, and shell history files are not connected. History comes from commands submitted in the current terminal. The GPUI view owns debounce, cancellation, stale-response checks, menu selection, and history inlays.

## Source and maintenance

[UPSTREAM.json](UPSTREAM.json) records the Warp source extraction at `1e6ef379bc9f51257c4243e8c4396a046cede8af`. The parser, matching engine, fuzzy helpers, and registry are maintained locally. Warp CLI registration is removed: first-use initialization no longer constructs a Clap application command tree or needs a temporary enlarged stack. Hosts may register additional signatures explicitly.

[SIGNATURES-UPSTREAM.json](SIGNATURES-UPSTREAM.json) records the command-signatures repository at `48fd7aa5b7b4d99f5db57d1c5a25c47c05567fcf`. `src/metadata.rs` and `src/catalog/` contain its schema, embedded lookup, and generator definitions; `signatures/` contains the embedded JSON rules. Generator definitions are data and parsing functions until a host supplies a generator context. Signature-generation tools are not included.

Review and port relevant upstream changes into these files, retaining their licenses and updating source records. Retain [LICENSE-AGPL](LICENSE-AGPL) and [LICENSE-MIT](LICENSE-MIT) for Warp sources and [LICENSE-SIGNATURES.md](LICENSE-SIGNATURES.md) for the MIT command-signatures sources and data. Run the scoped checks described in [terminal_core](../terminal_core/README.md#maintenance-and-licensing), including cold concurrent completion on small worker stacks.
