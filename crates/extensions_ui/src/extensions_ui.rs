mod components;
mod extension_suggestions;
mod extension_version_selector;

use std::sync::OnceLock;
use std::time::Duration;
use std::{any::TypeId, ops::Range, sync::Arc};

use anyhow::Context as _;
use cloud_api_types::{ExtensionMetadata, ExtensionProvides};
use collections::{BTreeMap, BTreeSet};
use command_palette_hooks::CommandPaletteFilter;
use extension_host::{ExtensionIndexEntry, ExtensionManifest, ExtensionStore};
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use git::{GitHostingProviderRegistry, parse_git_remote_url};
use gpui_icons::{Icon as MorphIcon, IconName as MenuIcon};
use gpui_kit::{
    Action, App, ClipboardItem, Context, DismissEvent, Entity, EventEmitter, Focusable,
    ParentElement, Render, SharedString, Styled, Task, TaskExt, UniformListScrollHandle,
    WeakEntity, Window, actions, div, point, prelude::*, px, rems, uniform_list,
};

use picker::{Picker, PickerDelegate};
use project::DirectoryLister;

use gpui_kit::{
    base::{Scrollbar, ScrollbarAxis, ScrollbarHandle as _, ScrollbarMode},
    component::{
        Icon as KitIcon, IconName as KitIconName, Selectable as _, Sizable as _,
        alert::Alert,
        button::{Button as KitButton, ButtonGroup, ButtonVariants as _},
        empty::{Empty, EmptyDescription, EmptyHeader, EmptyMedia},
        h_flex,
        input::{Input, InputEvent, InputState},
        label::Label as KitLabel,
        list::ListItem,
        menu::PopupMenuItem,
        switch::Switch,
        tab::{Tab, TabBar},
        v_flex,
    },
};
use schemars::JsonSchema;
use serde::Deserialize;
use settings::{Settings, SettingsContent};
use strum::IntoEnumIterator as _;
use ui::{ActiveTheme as _, IconName};
use util::ResultExt;
use vim_mode_setting::VimModeSetting;
use workspace::{
    Workspace,
    item::{Item, ItemEvent},
    workspace_error::{ErrorAction, ErrorSeverity, WorkspaceError},
};
use zed_actions::ExtensionCategoryFilter;

use crate::components::{ExtensionCard, extension_provides_label};
use crate::extension_version_selector::{
    ExtensionVersionSelector, ExtensionVersionSelectorDelegate,
};

actions!(
    zed,
    [
        /// Installs an extension from a local directory for development.
        InstallDevExtension,
    ]
);

/// Rebuilds an installed dev extension.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, JsonSchema, gpui_kit::Action)]
#[action(namespace = zed)]
#[serde(deny_unknown_fields)]
pub struct RebuildDevExtension {
    /// The ID of the dev extension to rebuild.
    ///
    /// Default: opens a picker if multiple dev extensions are installed.
    #[serde(default)]
    pub extension_id: Option<String>,
}

#[derive(Default)]
struct DevExtensionNotInstalledError {
    extension_id: Option<SharedString>,
}

impl WorkspaceError for DevExtensionNotInstalledError {
    fn primary_message(&self) -> SharedString {
        match &self.extension_id {
            Some(extension_id) => {
                format!("Dev extension '{extension_id}' is not installed.").into()
            }
            None => "No dev extensions are installed.".into(),
        }
    }

    fn primary_action(&self) -> ErrorAction {
        ErrorAction::new("Install Dev Extension", InstallDevExtension)
    }

    fn severity(&self) -> ErrorSeverity {
        ErrorSeverity::Warning
    }
}

fn update_rebuild_dev_extension_visibility(store: &Entity<ExtensionStore>, cx: &mut App) {
    let has_dev_extensions = store.read(cx).dev_extensions().next().is_some();
    CommandPaletteFilter::update_global(cx, |filter, _cx| {
        if has_dev_extensions {
            filter.show_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        } else {
            filter.hide_action_types(&[TypeId::of::<RebuildDevExtension>()]);
        }
    });
}

pub fn init(cx: &mut App) {
    let store = ExtensionStore::global(cx);
    update_rebuild_dev_extension_visibility(&store, cx);
    cx.observe(&store, |store, cx| {
        update_rebuild_dev_extension_visibility(&store, cx);
    })
    .detach();
    extension_suggestions::init(cx);

    cx.observe_new(move |workspace: &mut Workspace, window, _cx| {
        if window.is_none() {
            return;
        }
        workspace
            .register_action(
                move |workspace, action: &zed_actions::Extensions, window, cx| {
                    let provides_filter = action.category_filter.map(|category| match category {
                        ExtensionCategoryFilter::Themes => ExtensionProvides::Themes,
                        ExtensionCategoryFilter::IconThemes => ExtensionProvides::IconThemes,
                        ExtensionCategoryFilter::Languages => ExtensionProvides::Languages,
                        ExtensionCategoryFilter::Grammars => ExtensionProvides::Grammars,
                        ExtensionCategoryFilter::LanguageServers => {
                            ExtensionProvides::LanguageServers
                        }
                        ExtensionCategoryFilter::ContextServers => {
                            ExtensionProvides::ContextServers
                        }
                        ExtensionCategoryFilter::Snippets => ExtensionProvides::Snippets,
                        ExtensionCategoryFilter::DebugAdapters => ExtensionProvides::DebugAdapters,
                    });

                    let existing = workspace
                        .active_pane()
                        .read(cx)
                        .items()
                        .find_map(|item| item.downcast::<ExtensionsPage>());

                    if let Some(existing) = existing {
                        existing.update(cx, |extensions_page, cx| {
                            if provides_filter.is_some() {
                                extensions_page.change_provides_filter(provides_filter, cx);
                            }
                            if let Some(id) = action.id.as_ref() {
                                extensions_page.focus_extension(id, window, cx);
                            }
                        });

                        workspace.activate_item(&existing, true, true, window, cx);
                    } else {
                        let extensions_page = ExtensionsPage::new(
                            workspace,
                            provides_filter,
                            action.id.as_deref(),
                            window,
                            cx,
                        );
                        workspace.add_item_to_active_pane(
                            Box::new(extensions_page),
                            None,
                            true,
                            window,
                            cx,
                        )
                    }
                },
            )
            .register_action(move |workspace, _: &InstallDevExtension, window, cx| {
                let store = ExtensionStore::global(cx);
                let prompt = workspace.prompt_for_open_path(
                    gpui_kit::PathPromptOptions {
                        files: false,
                        directories: true,
                        multiple: false,
                        prompt: None,
                    },
                    DirectoryLister::Local(
                        workspace.project().clone(),
                        workspace.app_state().fs.clone(),
                    ),
                    window,
                    cx,
                );

                let workspace_handle = cx.entity().downgrade();
                window
                    .spawn(cx, async move |cx| {
                        let extension_path = match prompt.await.map_err(anyhow::Error::from) {
                            Ok(Some(mut paths)) => paths.pop()?,
                            Ok(None) => return None,
                            Err(err) => {
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        workspace.show_error(
                                            workspace::workspace_error::PortalError::new(
                                                err.to_string(),
                                            ),
                                            cx,
                                        );
                                    })
                                    .ok();
                                return None;
                            }
                        };

                        let install_task = store.update(cx, |store, cx| {
                            store.install_dev_extension(extension_path, cx)
                        });

                        match install_task.await {
                            Ok(_) => {}
                            Err(err) => {
                                log::error!("Failed to install dev extension: {:?}", err);
                                workspace_handle
                                    .update(cx, |workspace, cx| {
                                        // NOTE: using `anyhow::context` here ends up not printing
                                        // the error
                                        workspace.show_error(
                                            format!("Failed to install dev extension: {}", err),
                                            cx,
                                        );
                                    })
                                    .ok();
                            }
                        }

                        Some(())
                    })
                    .detach();
            })
            .register_action(move |workspace, action: &RebuildDevExtension, window, cx| {
                if let Some(target_id) = action.extension_id.as_deref() {
                    let extension_id = ExtensionStore::global(cx)
                        .read(cx)
                        .dev_extensions()
                        .find_map(|m| {
                            if m.id.as_ref() == target_id {
                                Some(m.id.clone())
                            } else {
                                None
                            }
                        });
                    if let Some(extension_id) = extension_id {
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    } else {
                        workspace.show_error(
                            DevExtensionNotInstalledError {
                                extension_id: Some(SharedString::from(target_id.to_owned())),
                            },
                            cx,
                        );
                    }
                    return;
                }

                let dev_extensions = ExtensionStore::global(cx)
                    .read(cx)
                    .dev_extensions()
                    .cloned()
                    .collect::<Vec<_>>();

                match dev_extensions.len() {
                    0 => {
                        workspace.show_error(DevExtensionNotInstalledError::default(), cx);
                    }
                    1 => {
                        let extension_id = dev_extensions[0].id.clone();
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id, cx);
                        });
                    }
                    _ => {
                        workspace.toggle_modal(window, cx, |window, cx| {
                            let delegate = DevExtensionRebuildPickerDelegate::new(dev_extensions);
                            Picker::uniform_list(delegate, window, cx)
                        });
                    }
                }
            });
    })
    .detach();
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum ExtensionFilter {
    All,
    Installed,
    NotInstalled,
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
enum Feature {
    AgentClaude,
    AgentCodex,
    AgentGemini,
    ExtensionBasedpyright,
    ExtensionRuff,
    ExtensionTailwind,
    ExtensionTy,
    Git,
    LanguageBash,
    LanguageC,
    LanguageCpp,
    LanguageGo,
    LanguagePython,
    LanguageReact,
    LanguageRust,
    LanguageTypescript,
    OpenIn,
    Vim,
}

fn keywords_by_feature() -> &'static BTreeMap<Feature, Vec<&'static str>> {
    static KEYWORDS_BY_FEATURE: OnceLock<BTreeMap<Feature, Vec<&'static str>>> = OnceLock::new();
    KEYWORDS_BY_FEATURE.get_or_init(|| {
        BTreeMap::from_iter([
            (
                Feature::AgentClaude,
                vec!["claude", "claude code", "claude agent"],
            ),
            (Feature::AgentCodex, vec!["codex", "codex cli"]),
            (Feature::AgentGemini, vec!["gemini", "gemini cli"]),
            (
                Feature::ExtensionBasedpyright,
                vec!["basedpyright", "pyright"],
            ),
            (Feature::ExtensionRuff, vec!["ruff"]),
            (Feature::ExtensionTailwind, vec!["tail", "tailwind"]),
            (Feature::ExtensionTy, vec!["ty"]),
            (Feature::Git, vec!["git"]),
            (Feature::LanguageBash, vec!["sh", "bash"]),
            (Feature::LanguageC, vec!["c", "clang"]),
            (Feature::LanguageCpp, vec!["c++", "cpp", "clang"]),
            (Feature::LanguageGo, vec!["go", "golang"]),
            (Feature::LanguagePython, vec!["python", "py"]),
            (Feature::LanguageReact, vec!["react"]),
            (Feature::LanguageRust, vec!["rust", "rs"]),
            (
                Feature::LanguageTypescript,
                vec!["type", "typescript", "ts"],
            ),
            (
                Feature::OpenIn,
                vec![
                    "github",
                    "gitlab",
                    "bitbucket",
                    "codeberg",
                    "sourcehut",
                    "permalink",
                    "link",
                    "open in",
                ],
            ),
            (Feature::Vim, vec!["vim"]),
        ])
    })
}

#[derive(Clone, Copy)]
enum DisplayedExtension {
    /// Index into the installed search results. `remote_index` points to the
    /// extension's metadata from the latest fetch, if it was part of it.
    Installed {
        index: usize,
        remote_index: Option<usize>,
    },
    /// Index into the remote extensions.
    Remote(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExtensionFetchState {
    Fetching,
    Succeeded,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FetchDebounce {
    Immediate,
    /// Debounce the fetch if there is a search query.
    WhenSearching,
}

pub struct ExtensionsPage {
    workspace: WeakEntity<Workspace>,
    extension_store: Entity<ExtensionStore>,
    provider_registry: Arc<GitHostingProviderRegistry>,
    list: UniformListScrollHandle,
    fetch_state: ExtensionFetchState,
    filter: ExtensionFilter,
    /// Installed extensions matching the current search.
    installed_search_results: Vec<ExtensionIndexEntry>,
    /// Results of the latest fetch, empty if it failed.
    remote_extensions: Vec<ExtensionMetadata>,
    displayed_extensions: Vec<DisplayedExtension>,
    query_input: Entity<InputState>,
    provides_filter: Option<ExtensionProvides>,
    _subscriptions: [gpui_kit::Subscription; 2],
    extension_fetch_task: Option<Task<()>>,
    local_search_task: Option<Task<()>>,
    upsells: BTreeSet<Feature>,
}

impl ExtensionsPage {
    pub fn new(
        workspace: &Workspace,
        provides_filter: Option<ExtensionProvides>,
        focus_extension_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let extension_store = ExtensionStore::global(cx);
            let workspace_handle = workspace.weak_handle();
            let subscriptions = [
                cx.observe(&extension_store, |_: &mut Self, _, cx| cx.notify()),
                cx.subscribe_in(
                    &extension_store,
                    window,
                    move |this, _, event, window, cx| match event {
                        extension_host::Event::ExtensionsUpdated => {
                            this.update_local_search_results(cx)
                        }
                        extension_host::Event::ExtensionInstalled(extension_id) => this
                            .on_extension_installed(
                                workspace_handle.clone(),
                                extension_id,
                                window,
                                cx,
                            ),
                        _ => {}
                    },
                ),
            ];

            let query_input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("Search extensions…")
                    .default_value(
                        focus_extension_id
                            .map(|id| format!("id:{id}"))
                            .unwrap_or_default(),
                    )
            });
            cx.subscribe(&query_input, Self::on_query_change).detach();

            let scroll_handle = UniformListScrollHandle::new();
            let provider_registry = GitHostingProviderRegistry::default_global(cx);

            let mut this = Self {
                workspace: workspace.weak_handle(),
                extension_store,
                provider_registry,
                list: scroll_handle,
                fetch_state: ExtensionFetchState::Fetching,
                filter: ExtensionFilter::All,
                installed_search_results: Vec::new(),
                remote_extensions: Vec::new(),
                displayed_extensions: Vec::new(),
                provides_filter,
                extension_fetch_task: None,
                local_search_task: None,
                _subscriptions: subscriptions,
                query_input,
                upsells: BTreeSet::default(),
            };
            this.update_local_search_results(cx);
            this.fetch_extensions(FetchDebounce::Immediate, None, cx);
            this
        })
    }

    fn get_repository_icon(&self, repository_url: &str) -> IconName {
        parse_git_remote_url(Arc::clone(&self.provider_registry), repository_url)
            .map(|(provider, _)| ui::git_hosting_provider_icon(provider.name().as_str()))
            .unwrap_or(IconName::Link)
    }

    fn on_extension_installed(
        &mut self,
        workspace: WeakEntity<Workspace>,
        extension_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let extension_store = self.extension_store.read(cx);
        let themes = extension_store
            .extension_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::theme_selector::Toggle {
                            themes_filter: Some(themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
            return;
        }

        let icon_themes = extension_store
            .extension_icon_themes(extension_id)
            .map(|name| name.to_string())
            .collect::<Vec<_>>();
        if !icon_themes.is_empty() {
            workspace
                .update(cx, |_workspace, cx| {
                    window.dispatch_action(
                        zed_actions::icon_theme_selector::Toggle {
                            themes_filter: Some(icon_themes),
                        }
                        .boxed_clone(),
                        cx,
                    );
                })
                .ok();
        }
    }

    /// Runs the search against the locally installed extensions, independently of
    /// the remote fetch, so local results show up without waiting for the server.
    fn update_local_search_results(&mut self, cx: &mut Context<Self>) {
        let search = self.search_query(cx);
        let mut installed_extensions = self
            .extension_store
            .read(cx)
            .installed_extensions()
            .values()
            .cloned()
            .collect::<Vec<_>>();

        self.local_search_task = Some(cx.spawn(async move |this, cx| {
            let results = match search.as_deref() {
                None => {
                    installed_extensions
                        .sort_by_cached_key(|extension| extension.manifest.name.to_lowercase());
                    installed_extensions
                }
                Some(search) => {
                    if let Some(extension_id) = search.strip_prefix("id:") {
                        installed_extensions
                            .into_iter()
                            .filter(|extension| extension.manifest.id.as_ref() == extension_id)
                            .collect()
                    } else {
                        let match_candidates = installed_extensions
                            .iter()
                            .enumerate()
                            .map(|(index, extension)| {
                                StringMatchCandidate::new(index, &extension.manifest.name)
                            })
                            .collect::<Vec<_>>();
                        let matches = match_strings(
                            &match_candidates,
                            search,
                            false,
                            true,
                            match_candidates.len(),
                            &Default::default(),
                            cx.background_executor().clone(),
                        )
                        .await;
                        matches
                            .into_iter()
                            .filter_map(|matched| {
                                installed_extensions.get(matched.candidate_id).cloned()
                            })
                            .collect()
                    }
                }
            };

            this.update(cx, |this, cx| {
                this.installed_search_results = results;
                this.rebuild_displayed_extensions(cx);
            })
            .ok();
        }));
    }

    fn rebuild_displayed_extensions(&mut self, cx: &mut Context<Self>) {
        let installed_extensions = self.extension_store.read(cx).installed_extensions();
        let provides_filter = self.provides_filter;

        // Remote results can lag behind a provides filter change by one fetch, so
        // the filter is applied locally as well.
        let installed_rows =
            self.installed_search_results
                .iter()
                .enumerate()
                .filter(|(_, extension)| {
                    provides_filter
                        .is_none_or(|provides| extension.manifest.provides().contains(&provides))
                });
        let remote_rows = self
            .remote_extensions
            .iter()
            .enumerate()
            .filter(|(_, extension)| {
                provides_filter
                    .is_none_or(|provides| extension.manifest.provides.contains(&provides))
            });
        let installed_row = |(index, extension): (usize, &ExtensionIndexEntry)| {
            let remote_index = (!extension.dev).then(|| {
                self.remote_extensions
                    .iter()
                    .position(|remote| remote.id == extension.manifest.id)
            });
            DisplayedExtension::Installed {
                index,
                remote_index: remote_index.flatten(),
            }
        };
        let remote_row = |(index, _)| DisplayedExtension::Remote(index);

        self.displayed_extensions = match self.filter {
            ExtensionFilter::All if self.fetch_state == ExtensionFetchState::Failed => {
                installed_rows.map(installed_row).collect()
            }
            ExtensionFilter::All => installed_rows
                .filter(|(_, extension)| extension.dev)
                .map(installed_row)
                .chain(remote_rows.map(remote_row))
                .collect(),
            ExtensionFilter::Installed => installed_rows.map(installed_row).collect(),
            ExtensionFilter::NotInstalled => remote_rows
                .filter(|(_, extension)| !installed_extensions.contains_key(&extension.id))
                .map(remote_row)
                .collect(),
        };
        cx.notify();
    }

    fn scroll_to_top(&mut self, cx: &mut Context<Self>) {
        self.list.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Fetches the remote extensions for the current search. Any fetch still in
    /// flight is cancelled.
    fn fetch_extensions(
        &mut self,
        debounce: FetchDebounce,
        on_complete: Option<Box<dyn FnOnce(&mut Self, &mut Context<Self>)>>,
        cx: &mut Context<Self>,
    ) {
        let search = self.search_query(cx);
        let provides_filter = BTreeSet::from_iter(self.provides_filter);
        self.fetch_state = ExtensionFetchState::Fetching;
        cx.notify();

        self.extension_fetch_task = Some(cx.spawn(async move |this, cx| {
            // Only debounce the fetching of extensions if we have a search
            // query.
            //
            // If the search was just cleared then we can just reload the list
            // of extensions without a debounce, which allows us to avoid seeing
            // an intermittent flash of a "no extensions" state.
            if debounce == FetchDebounce::WhenSearching && search.is_some() {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }

            let Ok(remote_extensions) = this.update(cx, |this, cx| {
                let extension_store = &this.extension_store;
                if let Some(id) = search
                    .as_deref()
                    .and_then(|search| search.strip_prefix("id:"))
                {
                    let versions = extension_store
                        .update(cx, |store, cx| store.fetch_extension_versions(id, cx));
                    cx.foreground_executor().spawn(async move {
                        let versions = versions.await?;
                        let latest = versions
                            .into_iter()
                            .max_by_key(|version| version.published_at)
                            .context("no extension found")?;
                        Ok(vec![latest])
                    })
                } else {
                    extension_store.update(cx, |store, cx| {
                        store.fetch_extensions(search.as_deref(), Some(&provides_filter), cx)
                    })
                }
            }) else {
                return;
            };
            let fetch_result = remote_extensions.await;

            this.update(cx, |this, cx| {
                match fetch_result {
                    Ok(remote_extensions) => {
                        this.fetch_state = ExtensionFetchState::Succeeded;
                        this.remote_extensions = remote_extensions;
                    }
                    Err(error) => {
                        log::error!("failed to fetch extensions: {error:#}");
                        this.fetch_state = ExtensionFetchState::Failed;
                        this.remote_extensions.clear();
                    }
                }
                this.rebuild_displayed_extensions(cx);
                if this.fetch_state == ExtensionFetchState::Succeeded
                    && let Some(on_complete) = on_complete
                {
                    on_complete(this, cx);
                }
            })
            .ok();
        }));
    }

    fn render_extensions(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<ExtensionCard> {
        let extension_store = self.extension_store.read(cx);
        range
            .filter_map(|index| {
                let row = *self.displayed_extensions.get(index)?;
                self.render_extension(row, extension_store, cx)
            })
            .collect()
    }

    fn render_extension(
        &self,
        row: DisplayedExtension,
        extension_store: &ExtensionStore,
        cx: &Context<Self>,
    ) -> Option<ExtensionCard> {
        let remote_index = match row {
            DisplayedExtension::Installed {
                index,
                remote_index: None,
            } => {
                let extension = self.installed_search_results.get(index)?;
                let manifest = extension.manifest.clone();
                let repository_icon = manifest
                    .repository
                    .as_deref()
                    .map(|url| self.get_repository_icon(url));
                let card = if extension.dev {
                    ExtensionCard::for_dev(manifest, extension_store)
                } else {
                    ExtensionCard::for_installed(manifest, extension_store)
                };
                return Some(match repository_icon {
                    Some(icon) => card.repository_icon(icon),
                    None => card,
                });
            }
            DisplayedExtension::Installed {
                remote_index: Some(remote_index),
                ..
            }
            | DisplayedExtension::Remote(remote_index) => remote_index,
        };

        let extension = self.remote_extensions.get(remote_index)?;
        let weak_self = cx.weak_entity();
        Some(
            ExtensionCard::for_remote(extension, extension_store, cx)
                .repository_icon(self.get_repository_icon(&extension.manifest.repository))
                .context_menu(move |menu, extension_id, authors, _, _| {
                    let weak_self = weak_self.clone();
                    menu.item(
                        PopupMenuItem::new("Install Another Version…")
                            .icon(workspace::menu_icon(MenuIcon::History))
                            .on_click({
                                let extension_id = extension_id.clone();
                                move |_, window, cx| {
                                    weak_self
                                        .update(cx, |this, cx| {
                                            this.show_extension_version_list(
                                                extension_id.clone(),
                                                window,
                                                cx,
                                            );
                                        })
                                        .log_err();
                                }
                            }),
                    )
                    .separator()
                    .item(
                        PopupMenuItem::new("Copy Extension ID")
                            .icon(workspace::menu_icon(MenuIcon::Copy))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    extension_id.to_string(),
                                ));
                            }),
                    )
                    .item(
                        PopupMenuItem::new("Copy Author Info")
                            .icon(workspace::menu_icon(MenuIcon::Person))
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    authors.to_string(),
                                ));
                            }),
                    )
                }),
        )
    }

    fn show_extension_version_list(
        &mut self,
        extension_id: Arc<str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(workspace) = self.workspace.upgrade() else {
            return;
        };

        cx.spawn_in(window, async move |this, cx| {
            let extension_versions_task = this.update(cx, |this, cx| {
                this.extension_store.update(cx, |store, cx| {
                    store.fetch_extension_versions(&extension_id, cx)
                })
            })?;

            let extension_versions = extension_versions_task.await?;

            workspace.update_in(cx, |workspace, window, cx| {
                let fs = workspace.project().read(cx).fs().clone();
                workspace.toggle_modal(window, cx, |window, cx| {
                    let delegate = ExtensionVersionSelectorDelegate::new(
                        fs,
                        cx.entity().downgrade(),
                        extension_versions,
                    );

                    ExtensionVersionSelector::new(delegate, window, cx)
                });
            })?;

            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_search(&self) -> impl IntoElement {
        Input::new(&self.query_input)
            .aria_label("Search extensions")
            .prefix(KitIcon::new(KitIconName::Search).size(rems(1.125)))
            .cleanable(true)
            .w_full()
            .when(self.fetch_state == ExtensionFetchState::Fetching, |input| {
                input.suffix(
                    MorphIcon::empty()
                        .id("extension-search-loading")
                        .path(IconName::LoadCircle.path())
                        .small()
                        .spinning(true),
                )
            })
    }

    fn retry_button(&self, cx: &mut Context<Self>) -> KitButton {
        KitButton::new("retry-fetch-extensions")
            .label("Retry")
            .outline()
            .on_click(cx.listener(|this, _, _, cx| {
                this.fetch_extensions(FetchDebounce::Immediate, None, cx);
            }))
    }

    fn on_query_change(
        &mut self,
        _: Entity<InputState>,
        event: &InputEvent,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) {
            self.refresh_search(cx);
        }
    }

    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        self.update_local_search_results(cx);
        self.fetch_extensions(
            FetchDebounce::WhenSearching,
            Some(Box::new(|this, cx| {
                this.scroll_to_top(cx);
            })),
            cx,
        );
        self.refresh_feature_upsells(cx);
    }

    pub fn focus_extension(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query_input.update(cx, |input, cx| {
            input.set_value(format!("id:{id}"), window, cx)
        });
        self.refresh_search(cx);
    }

    pub fn change_provides_filter(
        &mut self,
        provides_filter: Option<ExtensionProvides>,
        cx: &mut Context<Self>,
    ) {
        self.provides_filter = provides_filter;
        self.refresh_search(cx);
    }

    pub fn search_query(&self, cx: &mut App) -> Option<String> {
        let search = self.query_input.read(cx).value().to_string();
        if search.trim().is_empty() {
            None
        } else {
            Some(search)
        }
    }

    fn render_empty_state(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let has_search = self.search_query(cx).is_some();

        // The `Installed` filter is fully local, so fetch progress and failure are
        // only relevant to the other filters.
        let fetch_is_relevant = self.filter != ExtensionFilter::Installed;
        let fetch_failure_is_relevant =
            self.fetch_state == ExtensionFetchState::Failed && fetch_is_relevant;
        let is_loading = self.fetch_state == ExtensionFetchState::Fetching && fetch_is_relevant;
        let message = if is_loading {
            "Loading extensions…"
        } else if fetch_failure_is_relevant {
            "Failed to load extensions. Please check your connection and try again."
        } else {
            match self.filter {
                ExtensionFilter::All => {
                    if has_search {
                        "No extensions that match your search."
                    } else {
                        "No extensions."
                    }
                }
                ExtensionFilter::Installed => {
                    if has_search {
                        "No installed extensions that match your search."
                    } else {
                        "No installed extensions."
                    }
                }
                ExtensionFilter::NotInstalled => {
                    if has_search {
                        "No not installed extensions that match your search."
                    } else {
                        "No not installed extensions."
                    }
                }
            }
        };

        Empty::new()
            .size_full()
            .header(
                EmptyHeader::new()
                    .media(
                        EmptyMedia::new().child(
                            MorphIcon::new(if fetch_failure_is_relevant {
                                KitIconName::TriangleAlert
                            } else {
                                KitIconName::Search
                            })
                            .id("extension-empty-state")
                            .when(is_loading, |icon| icon.path(IconName::LoadCircle.path()))
                            .size(rems(1.5))
                            .spinning(is_loading),
                        ),
                    )
                    .description(EmptyDescription::new().child(message)),
            )
            .when(fetch_failure_is_relevant, |this| {
                this.child(self.retry_button(cx))
            })
    }

    fn update_settings(
        &mut self,
        selection: bool,
        cx: &mut Context<Self>,
        callback: impl 'static + Send + Fn(&mut SettingsContent, bool),
    ) {
        if let Some(workspace) = self.workspace.upgrade() {
            let fs = workspace.read(cx).app_state().fs.clone();
            settings::update_settings_file(fs, cx, move |settings, _| {
                callback(settings, selection)
            });
        }
    }

    fn refresh_feature_upsells(&mut self, cx: &mut Context<Self>) {
        let Some(search) = self.search_query(cx) else {
            self.upsells.clear();
            return;
        };

        if let Some(id) = search.strip_prefix("id:") {
            self.upsells.clear();

            let upsell = match id.to_lowercase().as_str() {
                "ruff" => Some(Feature::ExtensionRuff),
                "basedpyright" => Some(Feature::ExtensionBasedpyright),
                "ty" => Some(Feature::ExtensionTy),
                _ => None,
            };

            if let Some(upsell) = upsell {
                self.upsells.insert(upsell);
            }

            return;
        }

        let search = search.to_lowercase();
        let search_terms = search
            .split_whitespace()
            .map(|term| term.trim())
            .collect::<Vec<_>>();

        for (feature, keywords) in keywords_by_feature() {
            if keywords
                .iter()
                .any(|keyword| search_terms.contains(keyword))
            {
                self.upsells.insert(*feature);
            } else {
                self.upsells.remove(feature);
            }
        }
    }

    fn render_feature_upsell_banner(
        &self,
        label: &'static str,
        docs_url: &'static str,
        vim: bool,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .gap_2()
            .pt_3()
            .child(
                Alert::info(SharedString::from(format!("feature-{docs_url}")), label)
                    .min_w(rems(16.))
                    .flex_1(),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_3()
                    .child(
                        KitButton::new(SharedString::from(format!("docs-{docs_url}")))
                            .ghost()
                            .small()
                            .label("View Documentation")
                            .icon(KitIconName::ExternalLink)
                            .on_click(move |_, _, cx| {
                                telemetry::event!(
                                    "Documentation Viewed",
                                    source = "Feature Upsell",
                                    url = docs_url
                                );
                                cx.open_url(docs_url);
                            }),
                    )
                    .when(vim, |this| {
                        this.child(
                            Switch::new("enable-vim")
                                .label("Enable Vim mode")
                                .checked(VimModeSetting::get_global(cx).0)
                                .on_click(cx.listener(|this, selection: &bool, _, cx| {
                                    telemetry::event!(
                                        "Vim Mode Toggled",
                                        source = "Feature Upsell"
                                    );
                                    this.update_settings(*selection, cx, |settings, value| {
                                        settings.vim_mode = Some(value)
                                    });
                                })),
                        )
                    }),
            )
    }

    fn render_feature_upsells(&self, cx: &Context<Self>) -> impl IntoElement {
        v_flex().children(self.upsells.iter().map(|feature| match feature {
            Feature::AgentClaude => self.render_feature_upsell_banner(
                "Claude Agent support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#claude-agent",
                false,
                cx,
            ),
            Feature::AgentCodex => self.render_feature_upsell_banner(
                "Codex CLI support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#codex-cli",
                false,
                cx,
            ),
            Feature::AgentGemini => self.render_feature_upsell_banner(
                "Gemini CLI support is built-in to Zed!",
                "https://zed.dev/docs/ai/external-agents#gemini-cli",
                false,
                cx,
            ),
            Feature::ExtensionBasedpyright => self.render_feature_upsell_banner(
                "Basedpyright (Python language server) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python#basedpyright",
                false,
                cx,
            ),
            Feature::ExtensionRuff => self.render_feature_upsell_banner(
                "Ruff (linter for Python) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python#code-formatting--linting",
                false,
                cx,
            ),
            Feature::ExtensionTailwind => self.render_feature_upsell_banner(
                "Tailwind CSS support is built-in to Zed!",
                "https://zed.dev/docs/languages/tailwindcss",
                false,
                cx,
            ),
            Feature::ExtensionTy => self.render_feature_upsell_banner(
                "Ty (Python language server) support is built-in to Zed!",
                "https://zed.dev/docs/languages/python",
                false,
                cx,
            ),
            Feature::Git => self.render_feature_upsell_banner(
                "Zed comes with basic Git support—more features are coming in the future.",
                "https://zed.dev/docs/git",
                false,
                cx,
            ),
            Feature::LanguageBash => self.render_feature_upsell_banner(
                "Shell support is built-in to Zed!",
                "https://zed.dev/docs/languages/bash",
                false,
                cx,
            ),
            Feature::LanguageC => self.render_feature_upsell_banner(
                "C support is built-in to Zed!",
                "https://zed.dev/docs/languages/c",
                false,
                cx,
            ),
            Feature::LanguageCpp => self.render_feature_upsell_banner(
                "C++ support is built-in to Zed!",
                "https://zed.dev/docs/languages/cpp",
                false,
                cx,
            ),
            Feature::LanguageGo => self.render_feature_upsell_banner(
                "Go support is built-in to Zed!",
                "https://zed.dev/docs/languages/go",
                false,
                cx,
            ),
            Feature::LanguagePython => self.render_feature_upsell_banner(
                "Python support is built-in to Zed!",
                "https://zed.dev/docs/languages/python",
                false,
                cx,
            ),
            Feature::LanguageReact => self.render_feature_upsell_banner(
                "React support is built-in to Zed!",
                "https://zed.dev/docs/languages/typescript",
                false,
                cx,
            ),
            Feature::LanguageRust => self.render_feature_upsell_banner(
                "Rust support is built-in to Zed!",
                "https://zed.dev/docs/languages/rust",
                false,
                cx,
            ),
            Feature::LanguageTypescript => self.render_feature_upsell_banner(
                "Typescript support is built-in to Zed!",
                "https://zed.dev/docs/languages/typescript",
                false,
                cx,
            ),
            Feature::OpenIn => self.render_feature_upsell_banner(
                "Zed supports linking to a source line on GitHub and others.",
                "https://zed.dev/docs/git#git-integrations",
                false,
                cx,
            ),
            Feature::Vim => self.render_feature_upsell_banner(
                "Vim support is built-in to Zed!",
                "https://zed.dev/docs/vim",
                true,
                cx,
            ),
        }))
    }
}

struct DevExtensionRebuildPickerDelegate {
    entries: Vec<Arc<ExtensionManifest>>,
    matches: Vec<StringMatch>,
    selected_index: usize,
}

impl DevExtensionRebuildPickerDelegate {
    fn new(manifests: Vec<Arc<ExtensionManifest>>) -> Self {
        let matches = manifests
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatch {
                candidate_id: ix,
                score: 0.0,
                positions: Vec::new(),
                string: manifest.name.clone(),
            })
            .collect();

        Self {
            entries: manifests,
            matches,
            selected_index: 0,
        }
    }
}

impl PickerDelegate for DevExtensionRebuildPickerDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "dev-extension-rebuild"
    }

    fn match_count(&self) -> usize {
        self.matches.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) {
        self.selected_index = ix;
    }

    fn selected_index_changed(
        &self,
        _ix: usize,
        _window: &mut Window,
        _cx: &mut Context<Picker<Self>>,
    ) -> Option<Box<dyn Fn(&mut Window, &mut App) + 'static>> {
        None
    }

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background = cx.background_executor().clone();
        let candidates = self
            .entries
            .iter()
            .enumerate()
            .map(|(ix, manifest)| StringMatchCandidate::new(ix, manifest.name.as_ref()))
            .collect::<Vec<_>>();

        cx.spawn_in(window, async move |this, cx| {
            let matches = if query.is_empty() {
                candidates
                    .into_iter()
                    .enumerate()
                    .map(|(index, candidate)| StringMatch {
                        candidate_id: index,
                        string: candidate.string,
                        positions: Vec::new(),
                        score: 0.0,
                    })
                    .collect()
            } else {
                match_strings(
                    &candidates,
                    &query,
                    false,
                    true,
                    100,
                    &Default::default(),
                    background,
                )
                .await
            };

            this.update(cx, |this, _cx| {
                this.delegate.matches = matches;
                this.delegate.selected_index = this
                    .delegate
                    .selected_index
                    .min(this.delegate.matches.len().saturating_sub(1));
            })
            .log_err();
        })
    }

    fn confirm(&mut self, _secondary: bool, _window: &mut Window, cx: &mut Context<Picker<Self>>) {
        let Some(mat) = self.matches.get(self.selected_index) else {
            return;
        };

        let extension_id = self.entries[mat.candidate_id].id.clone();
        ExtensionStore::global(cx).update(cx, |store, cx| {
            store.rebuild_dev_extension(extension_id, cx);
        });

        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, _window: &mut Window, _cx: &mut Context<Picker<Self>>) {}

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        Arc::from("Rebuild dev extension…")
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let mat = self.matches.get(ix)?;
        let entry = self.entries.get(mat.candidate_id)?;

        let item = ListItem::new(("dev-extension-list-item", mat.candidate_id))
            .py_2()
            .selected(selected)
            .accessibility_label(entry.name.clone())
            .child(
                h_flex()
                    .w_full()
                    .py_px()
                    .justify_between()
                    .gap_2()
                    .child(KitLabel::new(entry.name.clone()))
                    .child(
                        KitLabel::new(format!("{} • v{}", entry.id, entry.version))
                            .text_sm()
                            .text_color(cx.theme().colors().text_muted),
                    ),
            );

        Some(item)
    }

    fn no_matches_text(&self, _window: &mut Window, _cx: &mut App) -> Option<SharedString> {
        Some("No dev extensions found".into())
    }
}

impl Render for ExtensionsPage {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.displayed_extensions.len();
        let categories = std::iter::once(None)
            .chain(
                ExtensionProvides::iter()
                    .filter(|provides| {
                        !matches!(
                            provides,
                            ExtensionProvides::AgentServers
                                | ExtensionProvides::Grammars
                                | ExtensionProvides::IndexedDocsProviders
                                | ExtensionProvides::SlashCommands
                        )
                    })
                    .map(Some),
            )
            .collect::<Vec<_>>();
        let selected_category = categories
            .iter()
            .position(|category| *category == self.provides_filter);

        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .overflow_hidden()
            .text_base()
            .bg(cx.theme().colors().editor_background)
            .child(
                v_flex()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .gap_4()
                    .pt_4()
                    .px_4()
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_3()
                            .justify_between()
                            .child(KitLabel::new("Extensions").text_xl())
                            .child(
                                KitButton::new("install-dev-extension")
                                    .outline()
                                    .label("Install Dev Extension")
                                    .flex_none()
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(InstallDevExtension), cx)
                                    }),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .flex_wrap()
                            .gap_2()
                            .child(div().flex_1().min_w(rems(16.)).child(self.render_search()))
                            .child(
                                ButtonGroup::new("extension-status")
                                    .outline()
                                    .flex_none()
                                    .children(
                                        [
                                            ("all", "All", ExtensionFilter::All),
                                            ("installed", "Installed", ExtensionFilter::Installed),
                                            (
                                                "not-installed",
                                                "Not Installed",
                                                ExtensionFilter::NotInstalled,
                                            ),
                                        ]
                                        .into_iter()
                                        .map(
                                            |(id, label, filter)| {
                                                KitButton::new(id)
                                                    .label(label)
                                                    .selected(self.filter == filter)
                                                    .on_click(cx.listener(move |this, _, _, cx| {
                                                        this.filter = filter;
                                                        this.rebuild_displayed_extensions(cx);
                                                        this.scroll_to_top(cx);
                                                    }))
                                            },
                                        ),
                                    ),
                            ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex_none()
                    .py_2()
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().colors().border_variant)
                    .overflow_hidden()
                    .child(
                        TabBar::new("extension-categories")
                            .pill()
                            .w_full()
                            .min_w_0()
                            .when_some(selected_category, |tabs, index| tabs.selected_index(index))
                            .children(categories.into_iter().map(|provides| {
                                let label = provides.map(extension_provides_label).unwrap_or("All");
                                Tab::new().label(label).on_click(cx.listener(
                                    move |this, _, _, cx| this.change_provides_filter(provides, cx),
                                ))
                            })),
                    ),
            )
            .child(
                v_flex()
                    .id("extension-feature-hints")
                    .w_full()
                    .px_4()
                    .max_h(rems(12.))
                    .flex_none()
                    .overflow_y_scroll()
                    .child(self.render_feature_upsells(cx)),
            )
            .when(
                self.fetch_state == ExtensionFetchState::Failed
                    && self.filter == ExtensionFilter::All
                    && count > 0,
                |this| {
                    this.child(
                        h_flex()
                            .w_full()
                            .flex_none()
                            .gap_3()
                            .px_4()
                            .pt_3()
                            .child(
                                Alert::warning(
                                    "extensions-offline",
                                    "Unable to load the catalog. Showing installed extensions.",
                                )
                                .flex_1(),
                            )
                            .child(self.retry_button(cx)),
                    )
                },
            )
            .child(
                v_flex()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .min_w_0()
                    .px_4()
                    .overflow_hidden()
                    .map(|this| {
                        if count == 0 {
                            this.child(self.render_empty_state(cx))
                        } else {
                            this.child(
                                uniform_list(
                                    "entries",
                                    count,
                                    cx.processor(Self::render_extensions),
                                )
                                .size_full()
                                .pb_4()
                                .track_scroll(&self.list),
                            )
                            .child(
                                div().absolute().inset_0().child(
                                    Scrollbar::new(&self.list)
                                        .axis(ScrollbarAxis::Vertical)
                                        .mode(ScrollbarMode::Scrolling)
                                        .viewport_from_layout(),
                                ),
                            )
                        }
                    }),
            )
    }
}

impl EventEmitter<ItemEvent> for ExtensionsPage {}

impl Focusable for ExtensionsPage {
    fn focus_handle(&self, cx: &App) -> gpui_kit::FocusHandle {
        self.query_input.focus_handle(cx)
    }
}

impl Item for ExtensionsPage {
    type Event = ItemEvent;

    fn tab_content_text(&self, _detail: usize, _cx: &App) -> SharedString {
        "Extensions".into()
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("Extensions Page Opened")
    }

    fn show_toolbar(&self) -> bool {
        false
    }

    fn to_item_events(event: &Self::Event, f: &mut dyn FnMut(workspace::item::ItemEvent)) {
        f(*event)
    }
}
