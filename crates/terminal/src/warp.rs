mod model;
mod pty;
pub(super) use model::{BackendTerm, Processor};
#[cfg(not(windows))]
pub(super) use pty::current_child_signal_mask;
pub(super) use pty::{PtySender, open_pty, pty_options, spawn_event_loop};

use crate::{
    Cell, Color, Content, Cursor, CursorShape, GridLinesChange, Hyperlink, IndexedCell, Modes,
    NamedColor, Point, PtyEvent, Range, RenderableCells, Rgb, Scroll, Search, Selection,
    SelectionRange, SelectionSide, SelectionType, TerminalBounds, ViMotion,
    terminal_settings::{AlternateScroll, CursorShape as SettingsCursorShape},
};
use futures::channel::mpsc::UnboundedSender;
use std::{
    marker::PhantomData,
    sync::Arc,
    time::{Duration, Instant},
};
use terminal_core::{
    SizeInfo,
    model::{
        TermMode,
        ansi::{self, Handler},
        char_or_str::CharOrStr,
        find::{FindConfig, RegexDFAs},
        grid::{Dimensions, RespectDisplayedOutput},
        index::Point as WarpPoint,
        secrets::RespectObfuscatedSecrets,
    },
};
use util::paths::PathStyle;

pub(super) type BackendTermLock = parking_lot::FairMutex<BackendTerm>;
pub(super) use terminal_core::model::cell::Flags as CellFlags;
#[derive(Clone, Debug)]
pub(super) struct BackendSearch(Arc<RegexDFAs>);
#[derive(Clone)]
pub(super) struct BackendTermConfig {
    pub scrolling_history: usize,
    cursor_shape: SettingsCursorShape,
}

pub(super) fn display_only_term_config(
    history: usize,
    cursor_shape: SettingsCursorShape,
) -> BackendTermConfig {
    BackendTermConfig {
        scrolling_history: history,
        cursor_shape,
    }
}
pub(super) fn pty_term_config(
    history: usize,
    cursor_shape: SettingsCursorShape,
) -> BackendTermConfig {
    display_only_term_config(history, cursor_shape)
}
pub(super) fn set_default_cursor_style(config: &mut BackendTermConfig, shape: SettingsCursorShape) {
    config.cursor_shape = shape;
}
fn cursor_style(shape: SettingsCursorShape) -> ansi::CursorStyle {
    ansi::CursorStyle {
        blinking: false,
        shape: match shape {
            SettingsCursorShape::Block => ansi::CursorShape::Block,
            SettingsCursorShape::Underline => ansi::CursorShape::Underline,
            SettingsCursorShape::Bar => ansi::CursorShape::Beam,
            SettingsCursorShape::Hollow => ansi::CursorShape::HollowBlock,
        },
    }
}
pub(super) fn apply_config(term: &BackendTermLock, config: &BackendTermConfig) {
    term.lock()
        .set_default_cursor_style(cursor_style(config.cursor_shape));
}
pub(super) fn size_info(bounds: TerminalBounds) -> SizeInfo {
    let mut size =
        SizeInfo::new_without_font_metrics(bounds.num_lines().max(1), bounds.num_columns().max(2));
    size.cell_width_px = terminal_core::units::Pixels::new(f32::from(bounds.cell_width()));
    size.cell_height_px = terminal_core::units::Pixels::new(f32::from(bounds.line_height()));
    size.pane_width_px = f32::from(bounds.cell_width()) * size.columns as f32;
    size.pane_height_px = f32::from(bounds.line_height()) * size.rows as f32;
    size
}
pub(super) fn new_term(
    config: &BackendTermConfig,
    bounds: TerminalBounds,
    events: UnboundedSender<PtyEvent>,
    alternate_scroll: AlternateScroll,
    generation: u64,
) -> Arc<BackendTermLock> {
    let mut term = BackendTerm::new(
        size_info(bounds),
        config.scrolling_history,
        events,
        generation,
    );
    term.set_default_cursor_style(cursor_style(config.cursor_shape));
    if let AlternateScroll::Off = alternate_scroll {
        term.alternate_scroll = false;
        term.primary.unset_mode(ansi::Mode::AlternateScroll);
        term.alternate.unset_mode(ansi::Mode::AlternateScroll);
    }
    Arc::new(BackendTermLock::new(term))
}
pub(super) fn resize(term: &mut BackendTerm, bounds: TerminalBounds) {
    term.resize(size_info(bounds));
}
pub(super) fn display_offset(term: &BackendTerm) -> usize {
    term.display_offset
}
pub(super) fn scroll_display(term: &mut BackendTerm, scroll: Scroll) {
    term.display_offset = match scroll {
        Scroll::Delta(delta) => term.display_offset.saturating_add_signed(delta as isize),
        Scroll::PageUp => term
            .display_offset
            .saturating_add(term.grid().visible_rows()),
        Scroll::PageDown => term
            .display_offset
            .saturating_sub(term.grid().visible_rows()),
        Scroll::Top => term.history_size(),
        Scroll::Bottom => 0,
    }
    .min(term.history_size());
}
fn absolute(term: &BackendTerm, point: Point) -> WarpPoint {
    WarpPoint::new(
        (point.line as i64 + term.history_size() as i64)
            .clamp(0, term.grid().total_rows().saturating_sub(1) as i64) as usize,
        point.column.min(term.grid().columns().saturating_sub(1)),
    )
}
fn relative(term: &BackendTerm, point: WarpPoint) -> Point {
    Point::new(point.row as i32 - term.history_size() as i32, point.col)
}
fn text_between(term: &BackendTerm, start: WarpPoint, end: WarpPoint) -> String {
    term.grid().bounds_to_string(
        start,
        end,
        false,
        RespectObfuscatedSecrets::No,
        false,
        RespectDisplayedOutput::No,
    )
}
pub(super) fn set_selection(term: &mut BackendTerm, selection: Option<&Selection>) {
    term.selection = selection.cloned();
}
pub(super) fn update_selection(term: &mut BackendTerm, point: Point, side: SelectionSide) -> bool {
    if let Some(selection) = &mut term.selection {
        selection.update(point, side);
        true
    } else {
        false
    }
}
fn selection_range(term: &BackendTerm) -> Option<SelectionRange> {
    let selection = term.selection.as_ref()?;
    let (mut start, mut end) = (selection.start, selection.end);
    if (start.point.line, start.point.column) > (end.point.line, end.point.column) {
        std::mem::swap(&mut start, &mut end);
    }
    let mut first = absolute(term, start.point);
    let mut last = absolute(term, end.point);
    match selection.ty {
        SelectionType::Simple => {
            if start.point == end.point && start.side == end.side {
                return None;
            }
            if first != last {
                let columns = term.grid().columns();
                let first_index = first.row * columns
                    + first.col
                    + usize::from(start.side == SelectionSide::Right);
                let end_exclusive =
                    last.row * columns + last.col + usize::from(end.side == SelectionSide::Right);
                if first_index >= end_exclusive {
                    return None;
                }
                first = WarpPoint::new(first_index / columns, first_index % columns);
                last = WarpPoint::new((end_exclusive - 1) / columns, (end_exclusive - 1) % columns);
            }
        }
        SelectionType::Semantic => {
            first = term.grid().semantic_search_left(first, |c| {
                c.is_whitespace() || ",│`|:\"' ()[]{}<>\t─".contains(c)
            });
            last = term.grid().semantic_search_right(last, |c| {
                c.is_whitespace() || ",│`|:\"' ()[]{}<>\t─".contains(c)
            });
        }
        SelectionType::Lines => {
            first = term.grid().line_search_left(first);
            last = term.grid().line_search_right(last);
        }
    }
    if first > last || last.row >= term.grid().total_rows() {
        return None;
    }
    Some(SelectionRange {
        start: relative(term, first),
        end: relative(term, last),
        is_block: false,
    })
}
pub(super) fn selection_text(term: &BackendTerm) -> Option<String> {
    let range = selection_range(term)?;
    Some(text_between(
        term,
        absolute(term, range.start),
        absolute(term, range.end),
    ))
}
pub(super) fn scroll_to_point(term: &mut BackendTerm, point: Point) {
    let top = -(term.display_offset as i32);
    let bottom = top + term.grid().visible_rows() as i32 - 1;
    if point.line < top {
        term.display_offset = (-point.line) as usize;
    }
    if point.line > bottom {
        term.display_offset = (term.grid().visible_rows() as i32 - 1 - point.line).max(0) as usize;
    }
    term.display_offset = term.display_offset.min(term.history_size());
}
pub(super) fn vi_goto_point(term: &mut BackendTerm, point: Point) {
    term.vi_cursor = Some(relative(term, absolute(term, point)));
    scroll_to_point(term, point);
}
pub(super) fn toggle_vi_mode(term: &mut BackendTerm) {
    term.vi_cursor = if term.vi_cursor.is_some() {
        None
    } else {
        Some(relative(term, term.grid().cursor_point()))
    };
}
pub(super) fn vi_motion(term: &mut BackendTerm, motion: ViMotion) {
    let Some(cursor) = term.vi_cursor else {
        return;
    };
    let mut point = absolute(term, cursor);
    let grid = term.grid();
    let columns = grid.columns();
    let last_index = grid.total_rows().saturating_mul(columns).saturating_sub(1);
    let at = |index: usize| WarpPoint::new(index / columns, index % columns);
    let character = |point: WarpPoint| {
        grid.row(point.row)
            .and_then(|row| row.get(point.col).map(|cell| cell.c))
            .unwrap_or('\0')
    };
    let blank = |point| matches!(character(point), '\0' | ' ' | '\t');
    let first_occupied = |row_index| {
        grid.row(row_index)
            .and_then(|row| {
                (0..columns).find(|&column| {
                    row.get(column)
                        .is_some_and(|cell| !matches!(cell.c, '\0' | ' ' | '\t'))
                })
            })
            .unwrap_or(0)
    };
    match motion {
        ViMotion::Up => point.row = point.row.saturating_sub(1),
        ViMotion::Down => point.row = (point.row + 1).min(grid.total_rows() - 1),
        ViMotion::Left => {
            if point.col == 0 && point.row > 0 && grid.row_wraps(point.row - 1) {
                point = WarpPoint::new(point.row - 1, columns - 1);
            } else {
                point.col = point.col.saturating_sub(1);
            }
        }
        ViMotion::Right => {
            if grid.cell_type(point) == Some(terminal_core::model::grid::CellType::WideChar) {
                point.col = (point.col + 1).min(columns - 1);
            }
            if point.col == columns - 1
                && grid.row_wraps(point.row)
                && point.row + 1 < grid.total_rows()
            {
                point = WarpPoint::new(point.row + 1, 0);
            } else {
                point.col = (point.col + 1).min(columns - 1);
            }
        }
        ViMotion::First => point = grid.line_search_left(point),
        ViMotion::Last => {
            let end = grid.line_search_right(point);
            let start = grid.line_search_left(point);
            let mut index = end.row * columns + end.col;
            let first = start.row * columns + start.col;
            while index > first && blank(at(index)) {
                index -= 1;
            }
            point = at(index);
        }
        ViMotion::FirstOccupied => point.col = first_occupied(point.row),
        ViMotion::High | ViMotion::Middle | ViMotion::Low => {
            let top = grid.history_size().saturating_sub(term.display_offset);
            let offset = match motion {
                ViMotion::High => 0,
                ViMotion::Middle => (grid.visible_rows() / 2).saturating_sub(1),
                _ => grid.visible_rows() - 1,
            };
            point.row = top + offset;
            point.col = first_occupied(point.row);
        }
        ViMotion::WordLeft | ViMotion::WordRight | ViMotion::WordRightEnd => {
            let mut index = point.row * columns + point.col;
            match motion {
                ViMotion::WordLeft => {
                    index = index.saturating_sub(1);
                    while index > 0 && blank(at(index)) {
                        index -= 1;
                    }
                    while index > 0 && !blank(at(index - 1)) {
                        index -= 1;
                    }
                }
                ViMotion::WordRight => {
                    while index < last_index && !blank(at(index)) {
                        index += 1;
                    }
                    while index < last_index && blank(at(index)) {
                        index += 1;
                    }
                }
                ViMotion::WordRightEnd => {
                    index = (index + 1).min(last_index);
                    while index < last_index && blank(at(index)) {
                        index += 1;
                    }
                    while index < last_index && !blank(at(index + 1)) {
                        index += 1;
                    }
                }
                _ => unreachable!(),
            }
            point = at(index);
        }
        ViMotion::Bracket => {
            if let Some(matched) = grid.bracket_search(point) {
                point = matched;
            }
        }
        ViMotion::ParagraphUp | ViMotion::ParagraphDown => {
            let down = matches!(motion, ViMotion::ParagraphDown);
            let mut seen_content = false;
            loop {
                let next = if down {
                    point.row + 1
                } else {
                    point.row.saturating_sub(1)
                };
                if next == point.row || next >= grid.total_rows() {
                    break;
                }
                point.row = next;
                let empty = (0..columns).all(|column| blank(WarpPoint::new(next, column)));
                if empty && seen_content {
                    break;
                }
                seen_content |= !empty;
            }
            point.col = 0;
        }
    }
    if grid.cell_type(point) == Some(terminal_core::model::grid::CellType::WideCharSpacer) {
        point.col = point.col.saturating_sub(1);
    }
    vi_goto_point(term, relative(term, point));
}

pub(super) fn update_vi_cursor_for_scroll(term: &mut BackendTerm, _scroll: Scroll) {
    if let Some(cursor) = term.vi_cursor {
        let top = -(term.display_offset as i32);
        term.vi_cursor = Some(Point::new(
            cursor
                .line
                .clamp(top, top + term.grid().visible_rows() as i32 - 1),
            cursor.column,
        ));
    }
}
pub(super) fn update_selection_to_vi_cursor(term: &mut BackendTerm) -> Option<Point> {
    let cursor = term.vi_cursor?;
    term.selection
        .as_mut()?
        .update(cursor, SelectionSide::Right);
    Some(cursor)
}
pub(super) fn clear_saved_screen(term: &mut BackendTerm) {
    term.grid_mut().clear_screen(ansi::ClearMode::ResetAndClear);
    term.display_offset = 0;
    term.selection = None;
}
pub(super) fn shrink_to_used(_term: &mut BackendTerm) { /* Warp allocates scrollback lazily. */
}
pub(super) fn used_lines(term: &BackendTerm) -> usize {
    term.grid()
        .content_len()
        .max(term.grid().cursor_point().row + 1)
}
pub(super) fn total_lines(term: &BackendTerm) -> usize {
    term.grid().total_rows()
}
pub(super) fn screen_lines(term: &BackendTerm) -> usize {
    term.grid().visible_rows()
}
pub(super) fn full_content_range(term: &BackendTerm) -> Range {
    Range::new(
        relative(term, WarpPoint::new(0, 0)),
        relative(
            term,
            WarpPoint::new(total_lines(term) - 1, term.grid().columns() - 1),
        ),
    )
}
pub(super) fn content_text(term: &BackendTerm) -> String {
    text_between(
        term,
        WarpPoint::new(0, 0),
        WarpPoint::new(total_lines(term) - 1, term.grid().columns() - 1),
    )
}
pub(super) fn last_non_empty_lines(term: &BackendTerm, count: usize) -> Vec<String> {
    let mut lines: Vec<_> = content_text(term)
        .lines()
        .rev()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .take(count)
        .map(str::to_owned)
        .collect();
    lines.reverse();
    lines
}
pub(super) unsafe fn append_text_to_term(term: &mut BackendTerm, lines: &[&str]) {
    let mut parser = Processor::new();
    parser.advance(term, format!("\r\n{}\r\n", lines.join("\r\n")).as_bytes());
}
impl Search {
    pub fn new(pattern: &str) -> Option<Self> {
        Some(Self {
            search: BackendSearch(Arc::new(
                RegexDFAs::new_with_config(
                    pattern,
                    FindConfig {
                        is_regex_enabled: true,
                        is_case_sensitive: true,
                    },
                )
                .ok()?,
            )),
        })
    }
}
pub(super) fn search_matches(term: &BackendTerm, search: Search) -> Vec<Range> {
    let mut matches: Vec<_> = term
        .grid()
        .find(&search.search.0)
        .map(|range| Range::new(relative(term, *range.start()), relative(term, *range.end())))
        .collect();
    matches.reverse();
    matches
}
fn modes(term: &BackendTerm) -> Modes {
    let mut modes = Modes::empty();
    macro_rules! copy_modes { ($($name:ident),* $(,)?) => { $(if term.grid().is_mode_set(TermMode::$name) { modes.insert(Modes::$name); })* }; }
    copy_modes!(
        APP_CURSOR,
        APP_KEYPAD,
        SHOW_CURSOR,
        LINE_WRAP,
        ORIGIN,
        INSERT,
        LINE_FEED_NEW_LINE,
        FOCUS_IN_OUT,
        ALTERNATE_SCROLL,
        BRACKETED_PASTE,
        SGR_MOUSE,
        UTF8_MOUSE,
        MOUSE_REPORT_CLICK,
        MOUSE_DRAG,
        MOUSE_MOTION
    );
    if term.alternate_active {
        modes.insert(Modes::ALT_SCREEN);
    }
    if term.vi_cursor.is_some() {
        modes.insert(Modes::VI);
    }
    modes
}
fn render_cells(term: &BackendTerm) -> Vec<IndexedCell> {
    let grid = term.grid();
    let top = grid.history_size().saturating_sub(term.display_offset);
    let mut cells = Vec::with_capacity(grid.visible_rows() * grid.columns());
    let mut hyperlinks = std::collections::HashMap::new();
    for row_index in top..(top + grid.visible_rows()).min(grid.total_rows()) {
        let Some(row) = grid.row(row_index) else {
            continue;
        };
        for column in 0..grid.columns() {
            let Some(source) = row.get(column) else {
                continue;
            };
            let point = WarpPoint::new(row_index, column);
            let hidden = source.flags.contains(CellFlags::HIDDEN);
            let zerowidth = match source.content_for_display() {
                CharOrStr::Str(text) if !hidden => Some(Arc::new(text.chars().skip(1).collect())),
                CharOrStr::Char(_) | CharOrStr::Str(_) => None,
            };
            let hyperlink = source.hyperlink_id().and_then(|id| {
                if let Some(hyperlink) = hyperlinks.get(&id) {
                    return Some(Hyperlink::clone(hyperlink));
                }
                let uri = grid.hyperlink_uri_at_point(point)?;
                let hyperlink = Hyperlink::new(None::<String>, uri.to_owned());
                hyperlinks.insert(id, hyperlink.clone());
                Some(hyperlink)
            });
            let cell = Cell {
                character: if source.c == '\0' || hidden {
                    ' '
                } else {
                    source.c
                },
                foreground: resolved_color(term, source.fg),
                background: resolved_color(term, source.bg),
                flags: source.flags,
                zerowidth,
                hyperlink,
            };
            cells.push(IndexedCell {
                point: relative(term, point),
                cell,
            });
        }
    }
    cells
}
pub(super) fn make_content(term: &BackendTerm, last: &Content) -> Content {
    let cells = render_cells(term);
    let changed = last.columns != term.grid().columns()
        || last.screen_lines != screen_lines(term)
        || last.total_lines != total_lines(term)
        || last.display_offset != term.display_offset
        || cells.len() != last.cells.len()
        || cells
            .iter()
            .zip(&last.cells)
            .any(|(a, b)| a.point != b.point || a.cell != b.cell);
    let point = term
        .vi_cursor
        .unwrap_or_else(|| relative(term, term.grid().cursor_render_point()));
    let cursor_char = cells
        .iter()
        .find(|cell| cell.point == point)
        .map_or(' ', |cell| cell.character());
    let mode = modes(term);
    let shape = if mode.contains(Modes::SHOW_CURSOR) || mode.contains(Modes::VI) {
        match term.cursor_style().shape {
            ansi::CursorShape::Block => CursorShape::Block,
            ansi::CursorShape::Underline => CursorShape::Underline,
            ansi::CursorShape::Beam => CursorShape::Bar,
            ansi::CursorShape::HollowBlock => CursorShape::HollowBlock,
            ansi::CursorShape::Hidden => CursorShape::Hidden,
        }
    } else {
        CursorShape::Hidden
    };
    let bottom_line = screen_lines(term) as i32 - 1 - term.display_offset as i32;
    let bottom_row_occupied = point.line >= bottom_line
        || cells
            .iter()
            .rev()
            .take_while(|cell| cell.point.line >= bottom_line)
            .any(|cell| cell.character() != ' ');
    Content {
        cells,
        mode,
        total_lines: total_lines(term),
        display_offset: term.display_offset,
        columns: term.grid().columns(),
        screen_lines: screen_lines(term),
        selection_text: selection_text(term),
        selection: selection_range(term),
        cursor: Cursor { point, shape },
        cursor_char,
        terminal_bounds: last.terminal_bounds,
        last_hovered_word: if changed {
            None
        } else {
            last.last_hovered_word.clone()
        },
        grid_lines_change: if changed {
            GridLinesChange::Changed
        } else {
            GridLinesChange::Unchanged
        },
        scrolled_to_top: term.display_offset == term.history_size(),
        scrolled_to_bottom: term.display_offset == 0,
        bottom_row_occupied,
    }
}
impl<'a> RenderableCells<'a> {
    pub(super) fn new(term: &'a BackendTerm) -> Self {
        Self {
            cells: render_cells(term).into_iter(),
            lifetime: PhantomData,
        }
    }
}
impl Iterator for RenderableCells<'_> {
    type Item = IndexedCell;
    fn next(&mut self) -> Option<Self::Item> {
        self.cells.next()
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.cells.size_hint()
    }
}

pub(super) struct RegexSearches {
    paths: Vec<regex::Regex>,
    timeout: Duration,
}
impl Default for RegexSearches {
    fn default() -> Self {
        Self::new(Vec::<String>::new(), Duration::ZERO)
    }
}
impl RegexSearches {
    pub fn new(patterns: impl IntoIterator<Item: AsRef<str>>, timeout: Duration) -> Self {
        Self {
            paths: patterns
                .into_iter()
                .filter_map(|pattern| match regex::Regex::new(pattern.as_ref()) {
                    Ok(regex) => Some(regex),
                    Err(error) => {
                        log::warn!("Invalid terminal path expression: {error}");
                        None
                    }
                })
                .collect(),
            timeout,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct HyperlinkMatch {
    pub text: String,
    pub is_url: bool,
    pub range: Range,
}
pub(super) fn find_from_terminal_point(
    term: &BackendTerm,
    point: Point,
    searches: &mut RegexSearches,
    path_style: PathStyle,
) -> Option<HyperlinkMatch> {
    let point = absolute(term, point);
    let grid = term.grid();
    if let Some(link) = grid
        .hyperlink_at_point(point)
        .or_else(|| grid.url_at_point(point))
    {
        let range = Range::new(
            relative(term, *link.range.start()),
            relative(term, *link.range.end()),
        );
        let text = grid
            .hyperlink_uri_at_point(point)
            .map(str::to_owned)
            .unwrap_or_else(|| text_between(term, *link.range.start(), *link.range.end()));
        return Some(normalize_hyperlink_match(text, true, range, path_style));
    }
    let start = grid.line_search_left(point);
    let end = grid.line_search_right(point);
    let mut text = String::new();
    let mut positions = Vec::new();
    for row_index in start.row..=end.row {
        let row = grid.row(row_index)?;
        for column in 0..grid.columns() {
            let cell = row.get(column)?;
            if cell.flags.intersects(
                terminal_core::model::cell::Flags::WIDE_CHAR_SPACER
                    | terminal_core::model::cell::Flags::LEADING_WIDE_CHAR_SPACER,
            ) {
                continue;
            }
            let content = cell.content_for_display().to_string();
            positions.extend(std::iter::repeat_n(
                WarpPoint::new(row_index, column),
                content.len(),
            ));
            text.push_str(&content);
        }
    }
    let started = Instant::now();
    for regex in &searches.paths {
        for found in regex.find_iter(&text) {
            if !searches.timeout.is_zero() && started.elapsed() > searches.timeout {
                return None;
            }
            let (Some(&start), Some(&end)) = (
                positions.get(found.start()),
                positions.get(found.end().checked_sub(1)?),
            ) else {
                continue;
            };
            if start <= point && point <= end {
                return Some(HyperlinkMatch {
                    text: found.as_str().to_owned(),
                    is_url: false,
                    range: Range::new(relative(term, start), relative(term, end)),
                });
            }
        }
    }
    None
}

impl Hyperlink {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        Self {
            id: id.map(|id| Arc::from(id.to_string())),
            uri: Arc::from(uri),
        }
    }
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }
    pub fn uri(&self) -> &str {
        &self.uri
    }
}
impl Default for Cell {
    fn default() -> Self {
        Self {
            character: ' ',
            foreground: Color::Named(NamedColor::Foreground),
            background: Color::Named(NamedColor::Background),
            flags: CellFlags::empty(),
            zerowidth: None,
            hyperlink: None,
        }
    }
}
impl Cell {
    pub fn character(&self) -> char {
        self.character
    }
    pub fn foreground(&self) -> Color {
        self.foreground
    }
    pub fn background(&self) -> Color {
        self.background
    }
    pub fn zerowidth(&self) -> Option<&[char]> {
        self.zerowidth.as_deref().map(Vec::as_slice)
    }
    pub fn hyperlink(&self) -> Option<Hyperlink> {
        self.hyperlink.clone()
    }
    pub fn is_inverse(&self) -> bool {
        self.flags.contains(CellFlags::INVERSE)
    }
    pub fn is_wide_char_spacer(&self) -> bool {
        self.flags.contains(CellFlags::WIDE_CHAR_SPACER)
    }
    pub fn is_dim(&self) -> bool {
        self.flags.contains(CellFlags::DIM)
    }
    pub fn has_underline(&self) -> bool {
        self.flags
            .intersects(CellFlags::UNDERLINE | CellFlags::DOUBLE_UNDERLINE)
    }
    pub fn has_undercurl(&self) -> bool {
        false
    }
    pub fn has_strikeout(&self) -> bool {
        self.flags.contains(CellFlags::STRIKEOUT)
    }
    pub fn is_bold(&self) -> bool {
        self.flags.contains(CellFlags::BOLD)
    }
    pub fn is_italic(&self) -> bool {
        self.flags.contains(CellFlags::ITALIC)
    }
    pub fn has_visible_style_modifier(&self) -> bool {
        self.flags.intersects(
            CellFlags::UNDERLINE
                | CellFlags::DOUBLE_UNDERLINE
                | CellFlags::INVERSE
                | CellFlags::STRIKEOUT,
        )
    }
    #[cfg(test)]
    pub(crate) fn set_character(&mut self, character: char) {
        self.character = character;
    }
    #[cfg(test)]
    pub(crate) fn push_zerowidth(&mut self, character: char) {
        Arc::make_mut(self.zerowidth.get_or_insert_with(Default::default)).push(character);
    }
}

fn color(color: ansi::Color) -> Color {
    match color {
        ansi::Color::Indexed(index) => Color::Indexed(index),
        ansi::Color::Spec(color) => Color::Spec(Rgb {
            r: color.r,
            g: color.g,
            b: color.b,
        }),
        ansi::Color::Named(color) => Color::Named(match color {
            ansi::NamedColor::Black => NamedColor::Black,
            ansi::NamedColor::Red => NamedColor::Red,
            ansi::NamedColor::Green => NamedColor::Green,
            ansi::NamedColor::Yellow => NamedColor::Yellow,
            ansi::NamedColor::Blue => NamedColor::Blue,
            ansi::NamedColor::Magenta => NamedColor::Magenta,
            ansi::NamedColor::Cyan => NamedColor::Cyan,
            ansi::NamedColor::White => NamedColor::White,
            ansi::NamedColor::BrightBlack => NamedColor::BrightBlack,
            ansi::NamedColor::BrightRed => NamedColor::BrightRed,
            ansi::NamedColor::BrightGreen => NamedColor::BrightGreen,
            ansi::NamedColor::BrightYellow => NamedColor::BrightYellow,
            ansi::NamedColor::BrightBlue => NamedColor::BrightBlue,
            ansi::NamedColor::BrightMagenta => NamedColor::BrightMagenta,
            ansi::NamedColor::BrightCyan => NamedColor::BrightCyan,
            ansi::NamedColor::BrightWhite => NamedColor::BrightWhite,
            ansi::NamedColor::Foreground => NamedColor::Foreground,
            ansi::NamedColor::Background => NamedColor::Background,
            ansi::NamedColor::Cursor => NamedColor::Cursor,
            ansi::NamedColor::DimBlack => NamedColor::DimBlack,
            ansi::NamedColor::DimRed => NamedColor::DimRed,
            ansi::NamedColor::DimGreen => NamedColor::DimGreen,
            ansi::NamedColor::DimYellow => NamedColor::DimYellow,
            ansi::NamedColor::DimBlue => NamedColor::DimBlue,
            ansi::NamedColor::DimMagenta => NamedColor::DimMagenta,
            ansi::NamedColor::DimCyan => NamedColor::DimCyan,
            ansi::NamedColor::DimWhite => NamedColor::DimWhite,
            ansi::NamedColor::BrightForeground => NamedColor::BrightForeground,
            ansi::NamedColor::DimForeground => NamedColor::DimForeground,
        }),
    }
}

use url::Url;
use util::paths::UrlExt;
fn normalize_hyperlink_match(
    maybe_url_or_path: String,
    is_url: bool,
    range: Range,
    path_style: PathStyle,
) -> HyperlinkMatch {
    if is_url {
        // Treat "file://" IRIs like file paths to ensure
        // that line numbers at the end of the path are
        // handled correctly.
        // Use Url::to_file_path() to properly handle Windows drive letters
        // (e.g., file:///C:/path -> C:\path)
        if maybe_url_or_path.starts_with("file://") {
            if let Ok(url) = Url::parse(&maybe_url_or_path) {
                if let Ok(path) = url.to_file_path_ext(path_style) {
                    return HyperlinkMatch {
                        text: path.to_string_lossy().into_owned(),
                        is_url: false,
                        range,
                    };
                } else if let Some(path) = try_osc8_url_to_path(url)
                    && path_style.is_posix()
                {
                    return HyperlinkMatch {
                        text: path,
                        is_url: false,
                        range,
                    };
                }
            }
            // Fallback: strip file:// prefix if URL parsing fails
            let path = maybe_url_or_path
                .strip_prefix("file://")
                .unwrap_or(&maybe_url_or_path);
            HyperlinkMatch {
                text: path.to_string(),
                is_url: false,
                range,
            }
        } else {
            HyperlinkMatch {
                text: maybe_url_or_path,
                is_url: true,
                range,
            }
        }
    } else {
        HyperlinkMatch {
            text: maybe_url_or_path,
            is_url: false,
            range,
        }
    }
}

// OSC 8 mandates that file:// URIs must be encoded as file://{host}{path}
// We need to skip the {host} part if it's set
fn try_osc8_url_to_path(url: url::Url) -> Option<String> {
    use percent_encoding::percent_decode;
    if url.scheme() != "file" {
        return None;
    }

    let bytes = url
        .path_segments()?
        .skip(1)
        .flat_map(|segment| percent_decode(segment.as_bytes()))
        .collect::<Vec<u8>>();
    bytes.try_into().ok()
}

fn resolved_color(term: &BackendTerm, value: ansi::Color) -> Color {
    let value = color(value);
    let index = match value {
        Color::Named(name) => Some(name as usize),
        Color::Indexed(index) => Some(index as usize),
        Color::Spec(_) => None,
    };
    index
        .and_then(|index| term.colors.get(index).copied().flatten())
        .map(Color::Spec)
        .unwrap_or(value)
}
