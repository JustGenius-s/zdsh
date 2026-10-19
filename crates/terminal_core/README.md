# Terminal core

This crate owns the ANSI/DCS parser, terminal grids and scrollback, cursor and mode state, hyperlinks, selection, search, and terminal protocol messages. It has no GPUI, WarpUI, font, application-state, PTY, or executor dependency. The host supplies `SizeInfo`, input bytes, and event queues; `terminal` handles process lifecycle and maps core cells into its public render data.

The implementation is extracted from Warp commit `1e6ef379bc9f51257c4243e8c4396a046cede8af`. [UPSTREAM.json](UPSTREAM.json) maps imported source files to upstream paths. The independent VTE parser dependency remains pinned to Warp's VTE fork at `4b399c87b63ba88f45709edaa6383fc519f6c900`; it has no Warp application or UI dependency.

## Host-independent types

Colors, pixel and line units, terminal dimensions, session identifiers, exit codes, and selection settings belong to this crate. GPUI conversions live in `terminal/src/warp.rs`; keyboard-event mapping remains in the host. Shell escaping and offset primitives are shared with `terminal_completion`. Parser diagnostics use local logging instead of Warp's telemetry services.

Hyperlinks and the upstream grid storage and resize fixes are enabled as fixed protocol policy. Image display and Kitty keyboard negotiation remain disabled. Image metadata parsing and placement calculations are retained, but asset caches, SVG/font loading, file-based image transfers, and shared-memory image transfers are absent. Secret matching has an empty default pattern set and is not configured from application settings.

## Maintenance and licensing

Source is maintained directly in zdsh. Compare the recorded upstream commit with a proposed update, port relevant changes into the affected files, update the source record, and run the scoped checks below. Replacing the source with a new Warp Git revision would reintroduce application dependencies.

Local adaptations remove the Warp runtime, process-global feature service, application events, shared sessions, and UI types. Upstream test helpers are omitted. Prompt metadata adds the optional `zdsh_environment` map for host-side environment probes; it uses the existing registered-session validation. The original parser/grid algorithms are retained; selection settings are plain values supplied by callers.

Retain [LICENSE-AGPL](LICENSE-AGPL), [LICENSE-ALACRITTY](LICENSE-ALACRITTY), and [LICENSE-MIT](LICENSE-MIT). Warp terminal sources are AGPL-3.0-only; Alacritty-derived files retain Apache-2.0 notices, and selected WarpUI value/protocol helpers retain their MIT license.

Run `./script/clippy -p terminal_core -p terminal_completion -p terminal -p terminal_view --locked` and `cargo build --locked -p zed --bin zed`. Review parser behavior, Unicode cells, scrollback/resizing, shell command boundaries, and first-use completion on a 512 KiB worker stack when changing the extraction.
