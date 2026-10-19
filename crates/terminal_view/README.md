# Terminal view

`terminal_view` renders the terminal crate's backend-neutral cells, cursor, selection, hyperlinks, and modes using GPUI. `terminal_core` owns ANSI processing and grids; `terminal` owns PTY transport, process lifecycle, and shell hooks; `terminal_completion` owns command parsing and matching. See [Terminal backend](../terminal/README.md) for dependency pins and supported shell integration.

## Command blocks

Standalone local Bash and Zsh shells use `src/command_blocks.rs`. Completed commands form a compact record separated by subtle horizontal rules. A single header row shows the command, home-relative working directory, and duration; nonzero exit codes are highlighted, and an unknown exit status is marked separately. Long commands and directories truncate with their full values available on hover. Collapse and copy actions appear when hovering over a command block without shifting the header. Output remains independently selectable and can be collapsed. Completed output expands to its full retained height, including outputs longer than 1,000 lines. Command output views use inline rendering without their own scrollbars or wheel scrolling; the surrounding command history owns scrolling. Only cells intersecting the visible viewport are painted. Running interactive programs retain a terminal viewport, and alternate-screen programs occupy the full terminal surface.

The compact input header shows the home-relative working directory, Git branch or detached HEAD, and project-relevant Rust, Node, Python, or virtual-environment labels. Tags use terminal theme colors, wrap at narrow widths, and show full values on hover. Environment queries run in the background and refresh after each command; outdated results are discarded. The permanent shortcut hint is omitted.

Command input, command headers, and standalone output share the terminal font family, size, weight, fallbacks, OpenType features, and line height. Unset terminal family, size, and fallbacks inherit the corresponding buffer settings. Input styling refreshes when settings change. Directory labels, environment tags, and status labels use the application UI typography. The bottom input uses Zed's editor for multiline editing and IME input. Enter runs the buffer, Shift+Enter inserts a newline, Up/Down browse in-session history at the first/last input line, Ctrl+P/Ctrl+N browse history directly, Ctrl+C clears the draft, and Ctrl+L clears completed blocks. The input buffer is quoted as one shell entry and evaluated in the interactive shell, preserving changes such as `cd` and exported variables.

`src/command_blocks/completions.rs` keeps typing quiet: only a longer matching command from in-session history appears as an inline hint. Right Arrow accepts the entire hint; Alt+Right accepts its next word. Enter runs only the actual buffer. Up Arrow with a nonempty prefix opens up to twelve distinct matching history entries, newest first; further typing filters that list. Plain history navigation retains the draft for restoration with Down Arrow.

Tab or Ctrl+Space requests core completions immediately. A single candidate is inserted directly. Multiple candidates extend a shared prefix when possible and open a compact, virtualized floating menu above the input without moving the command output. The first candidate is selected; Up/Down and Shift+Tab change selection, Enter/Tab accept it without running the command, and Escape or an outside click dismisses the menu. Editing closes a command completion menu. Completion edits preserve surrounding arguments and participate in editor undo.

A view-owned keystroke subscription handles command-input keys before editor key bindings. It acts only while this terminal is ready and its command editor is focused, and yields to IME composition and platform character input. Other editors retain their own bindings.

Pending queries are cancelled on edits, cursor moves, focus loss, and command execution; stale results are rejected before display and acceptance. IME composition retains its own keys. Hints and menus are hidden for selections, multiple cursors, and active commands. See the [backend completion scope](../terminal/README.md#completions) for available data sources.

The command editor remains mounted during execution, preserving its layout instead of replacing it with a status message. It is dimmed and read-only until the shell is ready; focusing it while busy redirects focus to the terminal. During command execution, keyboard input, paste, and Ctrl+C go to the PTY. Alternate-screen applications occupy the terminal area and return to the block document on exit. Focus returns to the command editor when the shell reports completion, provided the user has not focused another view.

Block integration is initialized only for editable standalone interactive terminals. Task, Agent-owned, embedded, read-only, and remote terminals retain the continuous terminal view. Command-block history is not persisted. Search and text selection are scoped to individual terminal surfaces.

## Rendering and input

`terminal_element.rs` lays out and paints terminal cells and handles terminal mouse protocols, local selection, hyperlinks, and IME text. The renderer depends on terminal-domain types rather than Warp implementation types. Completed block outputs reuse read-only `TerminalView` entities, so ANSI styling and text selection share the live renderer.

Mapped key events and GPUI actions use `Terminal::try_keystroke`; text, IME commits, and paste use `Terminal::input`. The independent command editor owns its text until submission. PTY creation remains fallible through `TerminalBuilder::new`, followed by `TerminalBuilder::subscribe` inside a GPUI entity context.
