//! One search panel over agent threads, recent projects, files, file contents, actions and
//! settings, each source behind a tab of its own and all but file contents together under "All".

use std::{
    mem,
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use agent_ui::thread_metadata_store::{ThreadMetadata, ThreadMetadataStore};
use anyhow::Context as _;
use command_palette::PaletteCommand;
use editor::Editor;
use fuzzy_nucleo::{Case, LengthPenalty, StringMatch, StringMatchCandidate};
use gpui::{
    Action, App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    HighlightStyle, KeyContext, ParentElement, Render, SharedString, Styled, StyledText,
    Subscription, Task, WeakEntity, Window, div, prelude::*, px, rems,
};
use gpui_icons::Icon;
use gpui_kit::{
    assets::IconName,
    base::Selectable as _,
    component::{
        ActiveTheme as _, IndexPath, Sizable as _,
        button::{Button, ButtonVariants as _},
        command::{Command, CommandGroup, CommandItem, CommandState},
        h_flex,
        input::{Input, InputEvent, InputState},
        kbd::Kbd,
        tab::{Tab, TabBar},
        v_flex,
    },
};
use language::{Anchor, Bias, Buffer, Point};
use project::{
    PathMatchCandidateSet, ProjectPath, WorktreeId,
    search::{SearchQuery, SearchResult as ProjectSearchResult},
};
use recent_projects::RecentProjectEntry;
use settings_ui::SearchableSetting;
use util::{
    ResultExt as _,
    paths::{PathMatcher, PathStyle},
};
use workspace::{DeploySearch, ModalView, OpenMode, Workspace, WorkspaceDb};
use zed_actions::unified_search::{
    OpenInProjectSearch, SearchTab, SelectNextTab, SelectPreviousTab, Toggle,
};

const TABS: [SearchTab; 7] = [
    SearchTab::All,
    SearchTab::Projects,
    SearchTab::Agents,
    SearchTab::Files,
    SearchTab::Text,
    SearchTab::Actions,
    SearchTab::Settings,
];

/// How many results each source contributes to the "All" tab.
const ALL_TAB_LIMIT: usize = 5;
/// How many results a source's own tab shows.
const TAB_LIMIT: usize = 100;
/// How many recently opened files the Files tab offers before anything is typed.
const RECENT_FILES_LIMIT: usize = 50;
/// How many matches the Text tab collects before it stops searching.
const TEXT_MATCH_LIMIT: usize = 200;
/// How long typing has to pause before the Text tab searches, since every search reads the
/// whole project.
const TEXT_SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);
/// How often matches found so far are shown while the Text tab is still searching.
const TEXT_SEARCH_FLUSH_INTERVAL: Duration = Duration::from_millis(100);
/// How much of the matched line the Text tab shows on either side of a match.
const MATCH_CONTEXT_BYTES: usize = 60;

pub fn init(cx: &mut App) {
    cx.observe_new(UnifiedSearch::register).detach();
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Source {
    Agents,
    Projects,
    Files,
    Text,
    /// The row that hands the Text tab's query over to Project Search.
    ProjectSearch,
    Actions,
    Settings,
}

impl Source {
    fn heading(self) -> &'static str {
        match self {
            Source::Agents => "Agents",
            Source::Projects => "Projects",
            Source::Files => "Files",
            Source::Text => "Text",
            Source::ProjectSearch => "Project Search",
            Source::Actions => "Actions",
            Source::Settings => "Settings",
        }
    }

    fn for_tab(tab: SearchTab) -> &'static [Source] {
        match tab {
            SearchTab::All => &[
                Source::Agents,
                Source::Projects,
                Source::Files,
                Source::Actions,
                Source::Settings,
            ],
            SearchTab::Projects => &[Source::Projects],
            SearchTab::Agents => &[Source::Agents],
            SearchTab::Files => &[Source::Files],
            SearchTab::Text => &[Source::Text],
            SearchTab::Actions => &[Source::Actions],
            SearchTab::Settings => &[Source::Settings],
        }
    }
}

fn tab_label(tab: SearchTab) -> &'static str {
    match tab {
        SearchTab::All => "All",
        SearchTab::Projects => "Projects",
        SearchTab::Agents => "Agents",
        SearchTab::Files => "Files",
        SearchTab::Text => "Text",
        SearchTab::Actions => "Actions",
        SearchTab::Settings => "Settings",
    }
}

/// A result, pointing into the source list it came from. Files are found per query rather than
/// listed up front, so they carry their own data.
#[derive(Clone, Debug)]
enum SearchResult {
    Thread(usize),
    Project(usize),
    File(FileResult),
    Command(usize),
    Setting(usize),
    TextMatch(usize),
    OpenInProjectSearch,
}

#[derive(Clone, Debug)]
struct FileResult {
    project_path: ProjectPath,
    file_name: SharedString,
    directory: SharedString,
}

/// A match of the Text tab's query in a file's contents.
#[derive(Clone, Debug)]
struct TextMatch {
    project_path: ProjectPath,
    file_name: SharedString,
    directory: SharedString,
    position: Point,
    /// The part of the matched line around the match.
    line: SharedString,
    /// Where the match lies in `line`.
    highlight: Range<usize>,
}

#[derive(Clone, Copy, Default)]
struct TextSearchOptions {
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
}

struct ResultGroup {
    source: Source,
    results: Vec<SearchResult>,
}

pub struct UnifiedSearch {
    workspace: WeakEntity<Workspace>,
    /// Where focus was before the panel opened: actions run there, and their keybindings are
    /// looked up there.
    previous_focus_handle: FocusHandle,
    query_input: Entity<InputState>,
    list: Entity<CommandState>,
    tab: SearchTab,
    threads: Vec<ThreadMetadata>,
    projects: Vec<RecentProjectEntry>,
    recent_files: Vec<FileResult>,
    commands: Vec<PaletteCommand>,
    settings: Vec<SearchableSetting>,
    text_matches: Vec<TextMatch>,
    text_options: TextSearchOptions,
    groups: Vec<ResultGroup>,
    search_cancel_flag: Arc<AtomicBool>,
    search_task: Task<()>,
    _load_projects: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ModalView for UnifiedSearch {}

impl EventEmitter<DismissEvent> for UnifiedSearch {}

impl Focusable for UnifiedSearch {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query_input.focus_handle(cx)
    }
}

impl UnifiedSearch {
    fn register(workspace: &mut Workspace, _: Option<&mut Window>, _: &mut Context<Workspace>) {
        workspace.register_action(|workspace, action: &Toggle, window, cx| {
            Self::toggle(workspace, action.tab, window, cx);
        });
    }

    /// Opens the panel on `tab`, or on "All" when no tab is given. When the panel is already
    /// open, a request for a different tab switches to it and any other request closes it.
    pub fn toggle(
        workspace: &mut Workspace,
        tab: Option<SearchTab>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        if let Some(search) = workspace.active_modal::<Self>(cx) {
            if let Some(tab) = tab
                && search.read(cx).tab != tab
            {
                search.update(cx, |search, cx| search.set_tab(tab, window, cx));
            } else {
                workspace.hide_modal(window, cx);
            }
            return;
        }

        if workspace.has_active_modal(window, cx) && !workspace.hide_modal(window, cx) {
            return;
        }

        let previous_focus_handle = window
            .focused(cx)
            .unwrap_or_else(|| workspace.focus_handle(cx));
        let workspace_handle = cx.entity().downgrade();
        let recent_files = recent_files(workspace, cx);
        let accounts_enabled = workspace.client().accounts_enabled();
        let current_workspace_id = workspace.database_id();
        let fs = workspace.app_state().fs.clone();
        workspace.toggle_modal(window, cx, move |window, cx| {
            let mut search = Self::new(
                workspace_handle,
                previous_focus_handle,
                tab.unwrap_or_default(),
                recent_files,
                accounts_enabled,
                window,
                cx,
            );

            let db = WorkspaceDb::global(cx);
            search._load_projects = cx.spawn_in(window, async move |this, cx| {
                let projects =
                    recent_projects::get_recent_projects(current_workspace_id, None, fs, &db).await;
                this.update_in(cx, |this, window, cx| {
                    this.projects = projects;
                    this.update_results(window, cx);
                })
                .log_err();
            });
            search
        });
    }

    fn new(
        workspace: WeakEntity<Workspace>,
        previous_focus_handle: FocusHandle,
        tab: SearchTab,
        recent_files: Vec<FileResult>,
        accounts_enabled: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let query_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search agents, files, and actions…")
        });
        let subscriptions = vec![cx.subscribe_in(
            &query_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.update_results(window, cx);
                }
            },
        )];

        // The panel is built before it takes focus, so this still lists the actions of the
        // element the user was in.
        let mut commands = command_palette::palette_commands(accounts_enabled, window, cx);
        let invocation_counts = command_palette::command_invocation_counts(cx);
        commands.sort_by(|a, b| {
            invocation_counts
                .get(&b.name)
                .cmp(&invocation_counts.get(&a.name))
                .then_with(|| a.name.cmp(&b.name))
        });

        let mut threads = ThreadMetadataStore::try_global(cx)
            .map(|store| {
                store
                    .read(cx)
                    .entries()
                    .filter(|thread| !thread.is_draft())
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        threads.sort_by_key(|thread| std::cmp::Reverse(thread.updated_at));

        let mut this = Self {
            workspace,
            previous_focus_handle,
            query_input,
            list: cx.new(|cx| CommandState::new(window, cx)),
            tab,
            threads,
            projects: Vec::new(),
            recent_files,
            commands,
            settings: settings_ui::searchable_settings(cx),
            text_matches: Vec::new(),
            text_options: TextSearchOptions::default(),
            groups: Vec::new(),
            search_cancel_flag: Arc::new(AtomicBool::new(false)),
            search_task: Task::ready(()),
            _load_projects: Task::ready(()),
            _subscriptions: subscriptions,
        };
        this.update_results(window, cx);
        this
    }

    fn query(&self, cx: &App) -> SharedString {
        self.query_input.read(cx).value()
    }

    fn set_tab(&mut self, tab: SearchTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.update_results(window, cx);
    }

    fn select_tab_by_offset(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        let current = TABS.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let next = (current as isize + offset).rem_euclid(TABS.len() as isize) as usize;
        if let Some(tab) = TABS.get(next) {
            self.set_tab(*tab, window, cx);
        }
    }

    fn select_previous_tab(
        &mut self,
        _: &SelectPreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_tab_by_offset(-1, window, cx);
    }

    fn select_next_tab(&mut self, _: &SelectNextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.select_tab_by_offset(1, window, cx);
    }

    fn update_results(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_cancel_flag.store(true, Ordering::Release);
        if self.tab == SearchTab::Text {
            self.search_text(window, cx);
            return;
        }

        let query = self.query(cx).trim().to_string();
        let sources = Source::for_tab(self.tab);
        let limit = if self.tab == SearchTab::All {
            ALL_TAB_LIMIT
        } else {
            TAB_LIMIT
        };

        if query.is_empty() {
            self.groups = sources
                .iter()
                .filter_map(|source| self.unfiltered_group(*source, limit))
                .collect();
            self.search_task = Task::ready(());
            self.reset_list(window, cx);
            return;
        }

        let cancel_flag = Arc::new(AtomicBool::new(false));
        self.search_cancel_flag = cancel_flag.clone();
        let string_candidates = sources
            .iter()
            .filter(|source| **source != Source::Files)
            .map(|source| (*source, self.string_candidates(*source)))
            .collect::<Vec<_>>();
        let path_candidates = sources
            .contains(&Source::Files)
            .then(|| self.path_candidates(cx))
            .unwrap_or_default();
        let command_query = command_palette::normalize_action_query(&query);
        let executor = cx.background_executor().clone();

        self.search_task = cx.spawn_in(window, async move |this, cx| {
            let case = Case::smart_if_uppercase_in(&query);
            let mut string_matches = Vec::with_capacity(string_candidates.len());
            for (source, candidates) in &string_candidates {
                let query = if *source == Source::Actions {
                    &command_query
                } else {
                    &query
                };
                let matches = fuzzy_nucleo::match_strings_async(
                    candidates,
                    query,
                    case,
                    LengthPenalty::On,
                    limit,
                    &cancel_flag,
                    executor.clone(),
                )
                .await;
                string_matches.push((*source, matches));
            }

            let path_matches = fuzzy_nucleo::match_path_sets(
                path_candidates.as_slice(),
                &query,
                &None,
                case,
                limit,
                &cancel_flag,
                executor.clone(),
            )
            .await;

            if cancel_flag.load(Ordering::Acquire) {
                return;
            }

            this.update_in(cx, |this, window, cx| {
                this.set_matches(sources, string_matches, path_matches, window, cx);
            })
            .log_err();
        });
    }

    fn unfiltered_group(&self, source: Source, limit: usize) -> Option<ResultGroup> {
        let results = match source {
            Source::Agents => (0..self.threads.len().min(limit))
                .map(SearchResult::Thread)
                .collect(),
            Source::Projects => (0..self.projects.len().min(limit))
                .map(SearchResult::Project)
                .collect(),
            Source::Files => self
                .recent_files
                .iter()
                .take(limit)
                .cloned()
                .map(SearchResult::File)
                .collect(),
            Source::Actions => (0..self.commands.len().min(limit))
                .map(SearchResult::Command)
                .collect(),
            // Listing every setting before anything is typed would bury the other sources.
            Source::Settings if self.tab == SearchTab::All => Vec::new(),
            // These are found per query, so there is nothing to list up front.
            Source::Text | Source::ProjectSearch => Vec::new(),
            Source::Settings => (0..self.settings.len().min(limit))
                .map(SearchResult::Setting)
                .collect(),
        };
        result_group(source, results)
    }

    fn string_candidates(&self, source: Source) -> Vec<StringMatchCandidate> {
        match source {
            Source::Agents => self
                .threads
                .iter()
                .enumerate()
                .map(|(ix, thread)| StringMatchCandidate::new(ix, &thread.display_title()))
                .collect(),
            Source::Projects => self
                .projects
                .iter()
                .enumerate()
                .map(|(ix, project)| StringMatchCandidate::new(ix, &project.name))
                .collect(),
            Source::Actions => self
                .commands
                .iter()
                .enumerate()
                .map(|(ix, command)| StringMatchCandidate::new(ix, &command.name))
                .collect(),
            Source::Settings => self
                .settings
                .iter()
                .enumerate()
                .map(|(ix, setting)| StringMatchCandidate::new(ix, &setting.title))
                .collect(),
            Source::Files | Source::Text | Source::ProjectSearch => Vec::new(),
        }
    }

    fn path_candidates(&self, cx: &App) -> Vec<PathMatchCandidateSet> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let worktree_store = workspace.read(cx).project().read(cx).worktree_store();
        let worktree_store = worktree_store.read(cx);
        let include_root_name = worktree_store.visible_worktrees(cx).count() > 1;
        worktree_store
            .visible_worktrees_and_single_files(cx)
            .map(|worktree| {
                let worktree = worktree.read(cx);
                PathMatchCandidateSet {
                    snapshot: worktree.snapshot(),
                    include_ignored: worktree.root_entry().is_some_and(|entry| entry.is_ignored),
                    include_root_name,
                    candidates: project::Candidates::Files,
                }
            })
            .collect()
    }

    fn set_matches(
        &mut self,
        sources: &[Source],
        string_matches: Vec<(Source, Vec<StringMatch>)>,
        path_matches: Vec<fuzzy_nucleo::PathMatch>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path_style = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).project().read(cx).path_style(cx));
        let mut string_matches = string_matches;
        let mut path_matches = Some(path_matches);
        self.groups = sources
            .iter()
            .filter_map(|source| {
                let results = if *source == Source::Files {
                    path_matches
                        .take()
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(|path_match| {
                            let path_style = path_style?;
                            let file_name = path_match.path.file_name()?.to_string();
                            let directory = path_match
                                .path_prefix
                                .join(path_match.path.parent()?)
                                .display(path_style)
                                .to_string();
                            Some(SearchResult::File(FileResult {
                                project_path: ProjectPath {
                                    worktree_id: WorktreeId::from_usize(path_match.worktree_id),
                                    path: path_match.path,
                                },
                                file_name: file_name.into(),
                                directory: directory.into(),
                            }))
                        })
                        .collect()
                } else {
                    let ix = string_matches
                        .iter()
                        .position(|(match_source, _)| match_source == source)?;
                    let (_, matches) = string_matches.swap_remove(ix);
                    matches
                        .into_iter()
                        .filter_map(|string_match| {
                            let ix = string_match.candidate_id;
                            match source {
                                Source::Agents => Some(SearchResult::Thread(ix)),
                                Source::Projects => Some(SearchResult::Project(ix)),
                                Source::Actions => Some(SearchResult::Command(ix)),
                                Source::Settings => Some(SearchResult::Setting(ix)),
                                Source::Files | Source::Text | Source::ProjectSearch => None,
                            }
                        })
                        .collect()
                };
                result_group(*source, results)
            })
            .collect();
        self.reset_list(window, cx);
    }

    fn search_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text_matches.clear();
        self.groups = self.text_groups(cx);
        self.reset_list(window, cx);
        self.search_task = Task::ready(());

        let query = self.query(cx).to_string();
        if query.is_empty() {
            return;
        }
        let Some(project) = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).project().clone())
        else {
            return;
        };
        let path_style = project.read(cx).path_style(cx);
        // Paths are matched in full when several worktrees could hold the same relative path.
        let match_full_paths = project.read(cx).visible_worktrees(cx).count() > 1;
        let options = self.text_options;
        let search_query = if options.regex {
            SearchQuery::regex(
                &query,
                options.whole_word,
                options.case_sensitive,
                false,
                false,
                PathMatcher::default(),
                PathMatcher::default(),
                match_full_paths,
                None,
            )
        } else {
            SearchQuery::text(
                &query,
                options.whole_word,
                options.case_sensitive,
                false,
                PathMatcher::default(),
                PathMatcher::default(),
                match_full_paths,
                None,
            )
        };
        // A regex is often invalid while it is being typed, which is no reason to report an error.
        let Ok(search_query) = search_query else {
            return;
        };

        self.search_task = cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(TEXT_SEARCH_DEBOUNCE).await;
            let search = project.update(cx, |project, cx| project.search(search_query, cx));
            // Dropping the search's task cancels it, so it lives as long as this loop.
            let _search_task = search.task_handle;
            let mut pending = Vec::new();
            let mut found = 0;
            let mut last_flush = Instant::now();
            while let Ok(result) = search.rx.recv().await {
                let (buffer, ranges) = match result {
                    ProjectSearchResult::Buffer { buffer, ranges } => (buffer, ranges),
                    ProjectSearchResult::LimitReached => break,
                    ProjectSearchResult::WaitingForScan | ProjectSearchResult::Searching => {
                        continue;
                    }
                };
                let Ok(matches) = this.update(cx, |_, cx| {
                    text_matches_in_buffer(&buffer, &ranges, path_style, cx)
                }) else {
                    return;
                };
                let take = matches.len().min(TEXT_MATCH_LIMIT - found);
                found += take;
                pending.extend(matches.into_iter().take(take));

                let limit_reached = found >= TEXT_MATCH_LIMIT;
                if limit_reached || last_flush.elapsed() >= TEXT_SEARCH_FLUSH_INTERVAL {
                    let batch = mem::take(&mut pending);
                    if this
                        .update(cx, |this, cx| this.append_text_matches(batch, cx))
                        .is_err()
                    {
                        return;
                    }
                    last_flush = Instant::now();
                }
                if limit_reached {
                    break;
                }
            }
            if !pending.is_empty() {
                this.update(cx, |this, cx| this.append_text_matches(pending, cx))
                    .log_err();
            }
        });
    }

    /// Adds matches to the Text tab. The groups only grow at their ends, so the selection stays
    /// on the result it was on.
    fn append_text_matches(&mut self, matches: Vec<TextMatch>, cx: &mut Context<Self>) {
        self.text_matches.extend(matches);
        self.groups = self.text_groups(cx);
        cx.notify();
    }

    fn text_groups(&self, cx: &App) -> Vec<ResultGroup> {
        let mut groups = Vec::new();
        groups.extend(result_group(
            Source::Text,
            (0..self.text_matches.len())
                .map(SearchResult::TextMatch)
                .collect(),
        ));
        if !self.query(cx).is_empty() {
            groups.push(ResultGroup {
                source: Source::ProjectSearch,
                results: vec![SearchResult::OpenInProjectSearch],
            });
        }
        groups
    }

    fn toggle_text_option(
        &mut self,
        option: fn(&mut TextSearchOptions) -> &mut bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let value = option(&mut self.text_options);
        *value = !*value;
        self.update_results(window, cx);
    }

    fn open_in_project_search(
        &mut self,
        _: &OpenInProjectSearch,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.tab != SearchTab::Text {
            return;
        }
        let options = self.text_options;
        let action = DeploySearch {
            query: Some(self.query(cx).to_string()),
            regex: Some(options.regex),
            case_sensitive: Some(options.case_sensitive),
            whole_word: Some(options.whole_word),
            ..DeploySearch::default()
        };
        self.dispatch_in_previous_focus(Box::new(action), window, cx);
    }

    /// Shows the current results, with the first one selected and scrolled to.
    fn reset_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The list keeps a selection by its position, so a fresh state is how the selection
        // returns to the first result when the results change.
        self.list = cx.new(|cx| CommandState::new(window, cx));
        cx.notify();
    }

    fn index_paths(&self) -> impl Iterator<Item = IndexPath> + '_ {
        self.groups.iter().enumerate().flat_map(|(section, group)| {
            (0..group.results.len()).map(move |row| IndexPath::new(row).section(section))
        })
    }

    fn select_by_offset(&mut self, offset: isize, window: &mut Window, cx: &mut Context<Self>) {
        let index_paths = self.index_paths().collect::<Vec<_>>();
        if index_paths.is_empty() {
            return;
        }
        let selected = self.list.read(cx).selected_index();
        let next = match selected.and_then(|selected| {
            index_paths
                .iter()
                .position(|index_path| *index_path == selected)
        }) {
            Some(ix) => (ix as isize + offset).rem_euclid(index_paths.len() as isize) as usize,
            None => 0,
        };
        let next = index_paths.get(next).copied();
        self.list
            .update(cx, |list, cx| list.set_selected_index(next, window, cx));
    }

    fn select_previous(
        &mut self,
        _: &menu::SelectPrevious,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select_by_offset(-1, window, cx);
    }

    fn select_next(&mut self, _: &menu::SelectNext, window: &mut Window, cx: &mut Context<Self>) {
        self.select_by_offset(1, window, cx);
    }

    fn confirm(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index_path) = self.list.read(cx).selected_index() {
            self.open(index_path, window, cx);
        }
    }

    fn cancel(&mut self, _: &menu::Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn open(&mut self, index_path: IndexPath, window: &mut Window, cx: &mut Context<Self>) {
        let Some(result) = self
            .groups
            .get(index_path.section)
            .and_then(|group| group.results.get(index_path.row))
            .cloned()
        else {
            return;
        };

        match result {
            SearchResult::Command(ix) => {
                let Some(command) = self.commands.get(ix) else {
                    return;
                };
                command_palette::record_command_invocation(
                    command.name.clone(),
                    self.query(cx).to_string(),
                    cx,
                );
                self.dispatch_in_previous_focus(command.action.boxed_clone(), window, cx);
            }
            SearchResult::Setting(ix) => {
                let Some(setting) = self.settings.get(ix) else {
                    return;
                };
                self.dispatch_in_previous_focus(setting.action.boxed_clone(), window, cx);
            }
            SearchResult::File(file) => {
                let workspace = self.workspace.clone();
                self.dismiss_then(window, cx, move |window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        workspace
                            .open_path(file.project_path, None, true, window, cx)
                            .detach_and_log_err(cx);
                    })
                });
            }
            SearchResult::Project(ix) => {
                let Some(project) = self.projects.get(ix) else {
                    return;
                };
                let paths = project.paths.clone();
                let workspace = self.workspace.clone();
                self.dismiss_then(window, cx, move |window, cx| {
                    let multi_workspace = workspace
                        .read_with(cx, |workspace, _| workspace.multi_workspace().cloned())?
                        .context("the workspace has no window to open the project in")?;
                    multi_workspace
                        .update(cx, |multi_workspace, cx| {
                            multi_workspace.open_project(paths, OpenMode::Activate, window, cx)
                        })?
                        .detach_and_log_err(cx);
                    anyhow::Ok(())
                });
            }
            SearchResult::TextMatch(ix) => {
                let Some(text_match) = self.text_matches.get(ix).cloned() else {
                    return;
                };
                let workspace = self.workspace.clone();
                self.dismiss_then(window, cx, move |window, cx| {
                    let open_task = workspace.update(cx, |workspace, cx| {
                        workspace.open_path(text_match.project_path, None, true, window, cx)
                    })?;
                    window
                        .spawn(cx, async move |cx| {
                            let item = open_task.await?;
                            if let Some(editor) = item.downcast::<Editor>() {
                                editor.update_in(cx, |editor, window, cx| {
                                    editor.go_to_singleton_buffer_point(
                                        text_match.position,
                                        window,
                                        cx,
                                    );
                                })?;
                            }
                            anyhow::Ok(())
                        })
                        .detach_and_log_err(cx);
                    anyhow::Ok(())
                });
            }
            SearchResult::OpenInProjectSearch => {
                self.open_in_project_search(&OpenInProjectSearch, window, cx);
            }
            SearchResult::Thread(ix) => {
                let Some(thread) = self.threads.get(ix).cloned() else {
                    return;
                };
                let workspace = self.workspace.clone();
                self.dismiss_then(window, cx, move |window, cx| {
                    let multi_workspace = workspace
                        .read_with(cx, |workspace, _| workspace.multi_workspace().cloned())?
                        .context("the workspace has no window to open the thread in")?
                        .upgrade()
                        .context("the workspace's window was closed")?;
                    let sidebar = multi_workspace
                        .read(cx)
                        .sidebar()
                        .and_then(|sidebar| sidebar.to_any().downcast::<sidebar::Sidebar>().ok())
                        .context("threads open through the threads sidebar, which is missing")?;
                    sidebar.update(cx, |sidebar, cx| sidebar.open_thread(thread, window, cx));
                    anyhow::Ok(())
                });
            }
        }
    }

    /// Runs `action` where focus was before the panel opened, as the command palette does, so
    /// that it reaches the editor or panel the user was in.
    fn dispatch_in_previous_focus(
        &mut self,
        action: Box<dyn Action>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.previous_focus_handle, cx);
        cx.emit(DismissEvent);
        window.dispatch_action(action, cx);
    }

    /// Closes the panel, then runs `open`. Closing first hands focus back before `open` moves it
    /// to what it opens.
    fn dismiss_then(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        open: impl FnOnce(&mut Window, &mut App) -> anyhow::Result<()> + 'static,
    ) {
        cx.emit(DismissEvent);
        window.defer(cx, move |window, cx| {
            open(window, cx).log_err();
        });
    }

    fn key_context(&self, cx: &App) -> KeyContext {
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("UnifiedSearch");
        // Left and right move through the query while it has text, and through the tabs once
        // it is empty.
        if self.query(cx).is_empty() {
            key_context.add("empty_query");
        }
        key_context
    }

    fn render_result(&self, result: &SearchResult, cx: &App) -> CommandItem {
        let (leading, label, secondary, detail) = match result {
            SearchResult::Thread(ix) => match self.threads.get(*ix) {
                Some(thread) => (
                    Leading::Dot,
                    thread.display_title(),
                    thread.archived.then(|| Secondary::plain("Archived")),
                    Detail::Text(sidebar::relative_time(thread.updated_at).into()),
                ),
                None => return CommandItem::new(),
            },
            SearchResult::Project(ix) => match self.projects.get(*ix) {
                Some(project) => (
                    Leading::Icon(IconName::Folder),
                    project.name.clone(),
                    Some(Secondary::plain(project.full_path.clone())),
                    Detail::Text(sidebar::relative_time(project.timestamp).into()),
                ),
                None => return CommandItem::new(),
            },
            SearchResult::File(file) => (
                Leading::Icon(IconName::File),
                file.file_name.clone(),
                Some(file.directory.clone())
                    .filter(|directory| !directory.is_empty())
                    .map(Secondary::plain),
                Detail::None,
            ),
            SearchResult::Command(ix) => match self.commands.get(*ix) {
                Some(command) => (
                    Leading::Icon(IconName::SquareTerminal),
                    command.name.clone(),
                    None,
                    Detail::Binding(
                        command.action.boxed_clone(),
                        self.previous_focus_handle.clone(),
                    ),
                ),
                None => return CommandItem::new(),
            },
            SearchResult::Setting(ix) => match self.settings.get(*ix) {
                Some(setting) => (
                    Leading::Icon(IconName::Settings),
                    setting.title.clone(),
                    Some(Secondary::plain(if setting.section.is_empty() {
                        setting.page.to_string()
                    } else {
                        format!("{} › {}", setting.page, setting.section)
                    })),
                    Detail::None,
                ),
                None => return CommandItem::new(),
            },
            SearchResult::TextMatch(ix) => match self.text_matches.get(*ix) {
                Some(text_match) => (
                    Leading::Icon(IconName::TextSearch),
                    SharedString::from(format!(
                        "{}:{}",
                        text_match.file_name,
                        text_match.position.row + 1
                    )),
                    Some(Secondary {
                        text: text_match.line.clone(),
                        highlight: text_match.highlight.clone(),
                    }),
                    Detail::Text(text_match.directory.clone()),
                ),
                None => return CommandItem::new(),
            },
            SearchResult::OpenInProjectSearch => (
                Leading::Icon(IconName::ExternalLink),
                SharedString::from("Open in Project Search"),
                Some(Secondary::plain("Replace, or search only some paths")),
                Detail::Binding(
                    Box::new(OpenInProjectSearch),
                    self.query_input.read(cx).focus_handle(cx),
                ),
            ),
        };

        CommandItem::new().child(move |window, cx| {
            let muted = cx.theme().muted_foreground;
            let foreground = cx.theme().foreground;
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_none()
                        .size_4()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(match leading {
                            Leading::Icon(icon) => Icon::new(icon)
                                .id("result-icon")
                                .small()
                                .text_color(muted)
                                .into_any_element(),
                            Leading::Dot => Icon::empty()
                                .id("result-icon")
                                .path(
                                    gpui_icons::IconName::Circle
                                        .path_with_variant(gpui_icons::Variant::Filled),
                                )
                                .size(px(6.))
                                .text_color(muted.opacity(0.5))
                                .into_any_element(),
                        }),
                )
                .child(
                    div()
                        .flex_none()
                        .max_w_2_3()
                        .truncate()
                        .child(label.clone()),
                )
                .children(secondary.clone().map(|secondary| {
                    let highlight = (!secondary.highlight.is_empty()).then(|| {
                        (
                            secondary.highlight.clone(),
                            HighlightStyle {
                                color: Some(foreground),
                                font_weight: Some(FontWeight::BOLD),
                                ..HighlightStyle::default()
                            },
                        )
                    });
                    div()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(muted)
                        .child(StyledText::new(secondary.text).with_highlights(highlight))
                }))
                .child(div().flex_1())
                .children(match &detail {
                    Detail::None => None,
                    Detail::Text(text) => Some(
                        div()
                            .flex_none()
                            .text_sm()
                            .text_color(muted)
                            .child(text.clone())
                            .into_any_element(),
                    ),
                    Detail::Binding(action, focus_handle) => {
                        Kbd::binding_for_action_in(action.as_ref(), focus_handle, window)
                            .map(|kbd| kbd.into_any_element())
                    }
                })
        })
    }

    fn render_text_option(
        &self,
        id: &'static str,
        icon: IconName,
        tooltip: &'static str,
        enabled: bool,
        option: fn(&mut TextSearchOptions) -> &mut bool,
        cx: &mut Context<Self>,
    ) -> Button {
        Button::new(id)
            .ghost()
            .xsmall()
            .icon(icon)
            .tooltip(tooltip)
            .selected(enabled)
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_text_option(option, window, cx);
            }))
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selected_tab = TABS.iter().position(|tab| *tab == self.tab).unwrap_or(0);
        let this = cx.entity().downgrade();
        h_flex()
            .flex_none()
            .px_2()
            .py_1p5()
            .gap_2()
            .child(
                TabBar::new("unified-search-tabs")
                    .pill()
                    .small()
                    .selected_index(selected_tab)
                    .children(TABS.iter().map(|tab| Tab::new().label(tab_label(*tab))))
                    .on_click(move |ix, window, cx| {
                        let Some(tab) = TABS.get(*ix).copied() else {
                            return;
                        };
                        this.update(cx, |this, cx| this.set_tab(tab, window, cx))
                            .log_err();
                    }),
            )
            .child(div().flex_1())
            .when(self.tab == SearchTab::Text, |row| {
                row.child(self.render_text_option(
                    "unified-search-case-sensitive",
                    IconName::CaseSensitive,
                    "Match Case",
                    self.text_options.case_sensitive,
                    |options| &mut options.case_sensitive,
                    cx,
                ))
                .child(self.render_text_option(
                    "unified-search-whole-word",
                    IconName::WholeWord,
                    "Match Whole Words",
                    self.text_options.whole_word,
                    |options| &mut options.whole_word,
                    cx,
                ))
                .child(self.render_text_option(
                    "unified-search-regex",
                    IconName::Regex,
                    "Use Regular Expression",
                    self.text_options.regex,
                    |options| &mut options.regex,
                    cx,
                ))
            })
            .child(
                Button::new("unified-search-previous-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowLeft)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.select_tab_by_offset(-1, window, cx);
                    })),
            )
            .child(
                Button::new("unified-search-next-tab")
                    .ghost()
                    .xsmall()
                    .icon(IconName::ArrowRight)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.select_tab_by_offset(1, window, cx);
                    })),
            )
    }
}

impl Render for UnifiedSearch {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let list = self.groups.iter().fold(
            Command::new(&self.list)
                .searchable(false)
                .filterable(false)
                .bordered(false)
                .max_h(rems(26.))
                .on_confirm(move |index_path, window, cx| {
                    this.update(cx, |this, cx| this.open(index_path, window, cx))
                        .log_err();
                }),
            |list, group| {
                list.group(
                    CommandGroup::new().label(group.source.heading()).items(
                        group
                            .results
                            .iter()
                            .map(|result| self.render_result(result, cx)),
                    ),
                )
            },
        );

        let theme = cx.theme();
        v_flex()
            .key_context(self.key_context(cx))
            .on_action(cx.listener(Self::select_previous))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .on_action(cx.listener(Self::select_previous_tab))
            .on_action(cx.listener(Self::select_next_tab))
            .on_action(cx.listener(Self::open_in_project_search))
            .w(rems(40.))
            .overflow_hidden()
            .bg(theme.popover)
            .text_color(theme.popover_foreground)
            .border_1()
            .border_color(theme.border)
            .rounded(theme.radius_lg)
            .shadow_lg()
            .child(
                div()
                    .flex_none()
                    .px_3()
                    .py_1()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Input::new(&self.query_input)
                            .appearance(false)
                            .prefix(Icon::new(IconName::Search).text_color(theme.muted_foreground)),
                    ),
            )
            .child(self.render_tabs(cx))
            .child(list)
    }
}

#[derive(Clone, Copy)]
enum Leading {
    Icon(IconName),
    /// The dot the threads sidebar marks threads with.
    Dot,
}

#[derive(Clone)]
struct Secondary {
    text: SharedString,
    /// The part of `text` to emphasize, such as a match within a line.
    highlight: Range<usize>,
}

impl Secondary {
    fn plain(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            highlight: 0..0,
        }
    }
}

enum Detail {
    None,
    Text(SharedString),
    /// The keybinding of an action, looked up where focus was before the panel opened.
    Binding(Box<dyn Action>, FocusHandle),
}

fn result_group(source: Source, results: Vec<SearchResult>) -> Option<ResultGroup> {
    (!results.is_empty()).then_some(ResultGroup { source, results })
}

fn recent_files(workspace: &Workspace, cx: &App) -> Vec<FileResult> {
    let project = workspace.project().read(cx);
    let path_style = project.path_style(cx);
    workspace
        .recent_navigation_history(Some(RECENT_FILES_LIMIT), cx)
        .into_iter()
        .filter_map(|(project_path, _)| {
            let file_name = project_path.path.file_name()?.to_string();
            let directory = project_path
                .path
                .parent()
                .map(|parent| parent.display(path_style).to_string())
                .unwrap_or_default();
            Some(FileResult {
                project_path,
                file_name: file_name.into(),
                directory: directory.into(),
            })
        })
        .collect()
}

fn text_matches_in_buffer(
    buffer: &Entity<Buffer>,
    ranges: &[Range<Anchor>],
    path_style: PathStyle,
    cx: &App,
) -> Vec<TextMatch> {
    let buffer = buffer.read(cx);
    let Some(file) = buffer.file() else {
        return Vec::new();
    };
    let project_path = ProjectPath {
        worktree_id: file.worktree_id(cx),
        path: file.path().clone(),
    };
    let Some(file_name) = project_path.path.file_name() else {
        return Vec::new();
    };
    let file_name = SharedString::from(file_name.to_string());
    let directory = SharedString::from(
        project_path
            .path
            .parent()
            .map(|parent| parent.display(path_style).to_string())
            .unwrap_or_default(),
    );

    let snapshot = buffer.snapshot();
    ranges
        .iter()
        .map(|range| {
            let start = snapshot.summary_for_anchor::<usize>(&range.start);
            let end = snapshot.summary_for_anchor::<usize>(&range.end);
            let position = snapshot.offset_to_point(start);
            let line_start = start - position.column as usize;
            let line_end =
                snapshot.point_to_offset(Point::new(position.row, snapshot.line_len(position.row)));
            // A bounded window around the match, so that a huge line costs no more than a short
            // one.
            let window_start = snapshot.clip_offset(
                start.saturating_sub(MATCH_CONTEXT_BYTES).max(line_start),
                Bias::Left,
            );
            let window_end = snapshot.clip_offset(
                end.saturating_add(MATCH_CONTEXT_BYTES)
                    .min(line_end)
                    .max(window_start),
                Bias::Right,
            );
            let window = snapshot
                .text_for_range(window_start..window_end)
                .collect::<String>();

            let indent = window.len() - window.trim_start().len();
            let mut line = String::new();
            if window_start > line_start {
                line.push('…');
            }
            let prefix = line.len();
            line.push_str(window[indent..].trim_end());
            let highlight_start = (start - window_start).saturating_sub(indent) + prefix;
            let highlight_end =
                (end.min(window_end) - window_start).saturating_sub(indent) + prefix;
            let highlight = if highlight_start <= highlight_end
                && line.is_char_boundary(highlight_start)
                && line.is_char_boundary(highlight_end)
            {
                highlight_start..highlight_end
            } else {
                0..0
            };

            TextMatch {
                project_path: project_path.clone(),
                file_name: file_name.clone(),
                directory: directory.clone(),
                position,
                line: line.into(),
                highlight,
            }
        })
        .collect()
}
