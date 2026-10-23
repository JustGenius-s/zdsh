use std::{collections::BTreeSet, sync::Arc};

use cloud_api_types::{ExtensionApiManifest, ExtensionMetadata, ExtensionProvides};
use component::{Component, ComponentScope, example_group, single_example};
use extension::{ExtensionManifest, SchemaVersion};
use extension_host::{ExtensionOperation, ExtensionStatus, ExtensionStore};
use gpui_icons::Icon as MorphIcon;
use gpui_kit::component::{
    Disableable as _, Icon as KitIcon, IconName as KitIconName, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    label::Label,
    menu::{DropdownMenu, PopupMenu},
    tag::Tag,
    v_flex,
};
use gpui_kit::{Anchor, AnyElement, App, ElementId, SharedString, Window, div, prelude::*, rems};
use release_channel::ReleaseChannel;
use ui::{ActiveTheme as _, IconName, RegisterComponent, utils::format_number_with_commas};

type ContextMenuBuilder =
    Box<dyn Fn(PopupMenu, Arc<str>, SharedString, &mut Window, &mut App) -> PopupMenu + 'static>;
type ExtensionCardActions = [Option<Button>; 3];

/// Local state of a published extension, derived from the `ExtensionStore`.
#[derive(Clone)]
enum PublishedExtensionState {
    /// A dev extension with the same id is installed locally and takes
    /// precedence, so the published version can't be installed or managed.
    /// This replaces the store status, which would describe the dev extension.
    OverriddenByDevExtension,
    /// Managed by the store like any other extension.
    Store(ExtensionStatus),
}

impl PublishedExtensionState {
    fn new(extension_id: &str, extension_store: &ExtensionStore) -> Self {
        if extension_store.is_dev_extension(extension_id) {
            Self::OverriddenByDevExtension
        } else {
            Self::Store(extension_store.extension_status(extension_id))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LocalExtensionKind {
    Dev,
    Installed,
}

struct ExtensionCardDetails {
    id: Arc<str>,
    name: SharedString,
    version: Arc<str>,
    description: Option<SharedString>,
    authors: SharedString,
    repository_url: Option<SharedString>,
    repository_icon: IconName,
    provided_features: Vec<&'static str>,
    source: ExtensionCardSource,
}

#[derive(Clone)]
enum ExtensionCardSource {
    Dev,
    Installed,
    Remote {
        state: PublishedExtensionState,
        download_count: u64,
    },
}

impl ExtensionCardSource {
    fn installed_version(&self, latest_version: &Arc<str>) -> Option<Arc<str>> {
        match self {
            Self::Remote {
                state: PublishedExtensionState::Store(ExtensionStatus::Installed(installed_version)),
                ..
            } if installed_version != latest_version => Some(installed_version.clone()),
            _ => None,
        }
    }

    fn download_count(&self) -> Option<u64> {
        match self {
            Self::Remote { download_count, .. } => Some(*download_count),
            Self::Dev | Self::Installed => None,
        }
    }

    fn is_dev(&self) -> bool {
        matches!(self, Self::Dev)
    }

    fn is_overridden(&self) -> bool {
        matches!(
            self,
            Self::Remote {
                state: PublishedExtensionState::OverriddenByDevExtension,
                ..
            }
        )
    }
}

#[derive(IntoElement, RegisterComponent)]
pub struct ExtensionCard {
    details: ExtensionCardDetails,
    actions: ExtensionCardActions,
    context_menu: Option<ContextMenuBuilder>,
}

impl ExtensionCard {
    pub fn for_dev(extension: Arc<ExtensionManifest>, extension_store: &ExtensionStore) -> Self {
        let status = extension_store.extension_status(&extension.id);
        Self::manifest(extension, status, LocalExtensionKind::Dev)
    }

    pub fn for_installed(
        extension: Arc<ExtensionManifest>,
        extension_store: &ExtensionStore,
    ) -> Self {
        let status = extension_store.extension_status(&extension.id);
        Self::manifest(extension, status, LocalExtensionKind::Installed)
    }

    pub fn for_remote(
        extension: &ExtensionMetadata,
        extension_store: &ExtensionStore,
        cx: &App,
    ) -> Self {
        let state = PublishedExtensionState::new(&extension.id, extension_store);
        Self::remote(extension, state, cx)
    }

    fn manifest(
        extension: Arc<ExtensionManifest>,
        status: ExtensionStatus,
        kind: LocalExtensionKind,
    ) -> Self {
        let actions = Self::actions_for_manifest_extension(&extension, &status, kind);
        let details = ExtensionCardDetails {
            id: extension.id.clone(),
            name: extension.name.clone().into(),
            version: extension.version.clone(),
            description: extension.description.clone().map(Into::into),
            authors: extension.authors.join(", ").into(),
            repository_url: extension.repository.clone().map(Into::into),
            repository_icon: IconName::Link,
            provided_features: provided_feature_labels(extension.provides()),
            source: match kind {
                LocalExtensionKind::Dev => ExtensionCardSource::Dev,
                LocalExtensionKind::Installed => ExtensionCardSource::Installed,
            },
        };

        Self {
            details,
            actions,
            context_menu: None,
        }
    }

    fn remote(extension: &ExtensionMetadata, state: PublishedExtensionState, cx: &App) -> Self {
        let actions = Self::actions_for_remote_extension(extension, &state, cx);
        let details = ExtensionCardDetails {
            id: extension.id.clone(),
            name: extension.manifest.name.clone().into(),
            version: extension.manifest.version.clone(),
            description: extension.manifest.description.clone().map(Into::into),
            authors: extension.manifest.authors.join(", ").into(),
            repository_url: Some(extension.manifest.repository.clone().into()),
            repository_icon: IconName::Link,
            provided_features: provided_feature_labels(extension.manifest.provides.iter().copied()),
            source: ExtensionCardSource::Remote {
                state,
                download_count: extension.download_count,
            },
        };

        Self {
            details,
            actions,
            context_menu: None,
        }
    }

    fn disables_actions(status: &ExtensionStatus) -> bool {
        match status {
            ExtensionStatus::Installing
            | ExtensionStatus::Upgrading
            | ExtensionStatus::Removing => true,
            ExtensionStatus::NotInstalled | ExtensionStatus::Installed(_) => false,
        }
    }

    fn button_id(extension_id: &Arc<str>, operation: ExtensionOperation) -> ElementId {
        (SharedString::from(extension_id.clone()), operation as usize).into()
    }

    fn uninstall_button(extension_id: &Arc<str>, is_dev: bool) -> Button {
        Button::new(Self::button_id(extension_id, ExtensionOperation::Remove))
            .label("Uninstall")
            .small()
            .on_click({
                let extension_id = extension_id.clone();
                move |_, _, cx| {
                    if !is_dev {
                        telemetry::event!("Extension Uninstalled", extension_id);
                    }
                    ExtensionStore::global(cx).update(cx, |store, cx| {
                        store
                            .uninstall_extension(extension_id.clone(), cx)
                            .detach_and_log_err(cx);
                    });
                }
            })
    }

    fn configure_button(
        extension_id: &Arc<str>,
        manifest: Option<Arc<ExtensionManifest>>,
    ) -> Button {
        Button::new(SharedString::from(format!("configure-{extension_id}")))
            .label("Configure")
            .small()
            .on_click({
                let extension_id = extension_id.clone();
                move |_, _, cx| {
                    let manifest = manifest.clone().or_else(|| {
                        ExtensionStore::global(cx)
                            .read(cx)
                            .extension_manifest_for_id(&extension_id)
                            .cloned()
                    });
                    if let Some(manifest) = manifest
                        && let Some(events) = extension::ExtensionEvents::try_global(cx)
                    {
                        events.update(cx, |this, cx| {
                            this.emit(extension::Event::ConfigureExtensionRequested(manifest), cx)
                        });
                    }
                }
            })
    }

    fn actions_for_manifest_extension(
        extension: &Arc<ExtensionManifest>,
        status: &ExtensionStatus,
        kind: LocalExtensionKind,
    ) -> ExtensionCardActions {
        let is_dev = kind == LocalExtensionKind::Dev;
        let rebuild = is_dev.then(|| {
            Button::new(SharedString::from(format!("rebuild-{}", extension.id)))
                .label("Rebuild")
                .small()
                .primary()
                .disabled(Self::disables_actions(status))
                .on_click({
                    let extension_id = extension.id.clone();
                    move |_, _, cx| {
                        ExtensionStore::global(cx).update(cx, |store, cx| {
                            store.rebuild_dev_extension(extension_id.clone(), cx)
                        });
                    }
                })
        });
        let uninstall = Self::uninstall_button(&extension.id, is_dev)
            .outline()
            .disabled(Self::disables_actions(status));
        let configure = (!extension.context_servers.is_empty()).then(|| {
            Self::configure_button(&extension.id, Some(extension.clone()))
                .outline()
                .disabled(Self::disables_actions(status))
        });

        if is_dev {
            [rebuild, Some(uninstall), configure]
        } else {
            [None, configure, Some(uninstall)]
        }
    }

    fn install_button(extension_id: &Arc<str>) -> Button {
        Button::new(Self::button_id(extension_id, ExtensionOperation::Install))
            .label("Install")
            .small()
            .primary()
            .icon(
                KitIcon::empty()
                    .path(IconName::Download.path())
                    .size(rems(1.125)),
            )
            .on_click({
                let extension_id = extension_id.clone();
                move |_, _, cx| {
                    telemetry::event!("Extension Installed");
                    ExtensionStore::global(cx).update(cx, |store, cx| {
                        store.install_latest_extension(extension_id.clone(), cx)
                    });
                }
            })
    }

    fn actions_for_remote_extension(
        extension: &ExtensionMetadata,
        state: &PublishedExtensionState,
        cx: &App,
    ) -> ExtensionCardActions {
        let is_configurable = extension
            .manifest
            .provides
            .contains(&ExtensionProvides::ContextServers);

        let status = match state {
            PublishedExtensionState::OverriddenByDevExtension => {
                return [
                    None,
                    None,
                    Some(Self::install_button(&extension.id).disabled(true)),
                ];
            }
            PublishedExtensionState::Store(status) => status,
        };

        match status {
            ExtensionStatus::NotInstalled | ExtensionStatus::Installing => [
                None,
                None,
                Some(Self::install_button(&extension.id).disabled(Self::disables_actions(status))),
            ],
            ExtensionStatus::Upgrading | ExtensionStatus::Removing => {
                let uninstall = Self::uninstall_button(&extension.id, false)
                    .outline()
                    .disabled(Self::disables_actions(status));
                let upgrade = matches!(status, ExtensionStatus::Upgrading).then(|| {
                    Button::new(Self::button_id(&extension.id, ExtensionOperation::Upgrade))
                        .label("Upgrade")
                        .small()
                        .disabled(Self::disables_actions(status))
                });
                let configure = is_configurable.then(|| {
                    Self::configure_button(&extension.id, None)
                        .disabled(Self::disables_actions(status))
                });

                [upgrade, configure, Some(uninstall)]
            }
            ExtensionStatus::Installed(installed_version) => {
                let uninstall = Self::uninstall_button(&extension.id, false).outline();
                let upgrade = (installed_version != &extension.manifest.version).then(|| {
                    let is_compatible = extension_host::is_version_compatible(
                        ReleaseChannel::global(cx),
                        extension,
                    );
                    Button::new(Self::button_id(&extension.id, ExtensionOperation::Upgrade))
                        .label("Upgrade")
                        .small()
                        .primary()
                        .when(!is_compatible, |button| {
                            button.tooltip(format!(
                                "v{} is not compatible with this version of zdsh.",
                                extension.manifest.version
                            ))
                        })
                        .disabled(!is_compatible)
                        .on_click({
                            let extension_id = extension.id.clone();
                            let version = extension.manifest.version.clone();
                            move |_, _, cx| {
                                telemetry::event!("Extension Installed", extension_id, version);
                                ExtensionStore::global(cx).update(cx, |store, cx| {
                                    store
                                        .upgrade_extension(
                                            extension_id.clone(),
                                            version.clone(),
                                            cx,
                                        )
                                        .detach_and_log_err(cx)
                                });
                            }
                        })
                });
                let configure =
                    is_configurable.then(|| Self::configure_button(&extension.id, None).outline());

                [upgrade, configure, Some(uninstall)]
            }
        }
    }

    pub fn repository_icon(mut self, icon: IconName) -> Self {
        self.details.repository_icon = icon;
        self
    }

    pub fn context_menu(
        mut self,
        builder: impl Fn(PopupMenu, Arc<str>, SharedString, &mut Window, &mut App) -> PopupMenu
        + 'static,
    ) -> Self {
        self.context_menu = Some(Box::new(builder));
        self
    }
}

fn provided_feature_labels(
    provides: impl IntoIterator<Item = ExtensionProvides>,
) -> Vec<&'static str> {
    provides
        .into_iter()
        .filter(|provides| !provides.is_deprecated())
        .map(extension_provides_label)
        .collect()
}

pub(crate) fn extension_provides_label(provides: ExtensionProvides) -> &'static str {
    match provides {
        ExtensionProvides::Themes => "Themes",
        ExtensionProvides::IconThemes => "Icon Themes",
        ExtensionProvides::Languages => "Languages",
        ExtensionProvides::Grammars => "Grammars",
        ExtensionProvides::LanguageServers => "Language Servers",
        ExtensionProvides::ContextServers => "MCP Servers",
        ExtensionProvides::AgentServers => "Agent Servers",
        ExtensionProvides::SlashCommands => "Slash Commands",
        ExtensionProvides::IndexedDocsProviders => "Indexed Docs Providers",
        ExtensionProvides::Snippets => "Snippets",
        ExtensionProvides::DebugAdapters => "Debug Adapters",
    }
}

impl Component for ExtensionCard {
    fn scope() -> ComponentScope {
        ComponentScope::DataDisplay
    }

    fn description() -> &'static str {
        "A card that displays an extension's details, installation state, and available actions."
    }

    fn preview(_window: &mut Window, cx: &mut App) -> AnyElement {
        fn remote_extension(
            id: &'static str,
            name: &'static str,
            version: &'static str,
            description: &'static str,
            download_count: u64,
            provides: impl IntoIterator<Item = ExtensionProvides>,
        ) -> ExtensionMetadata {
            ExtensionMetadata {
                id: id.into(),
                manifest: ExtensionApiManifest {
                    name: name.to_owned(),
                    version: version.into(),
                    description: Some(description.to_owned()),
                    authors: vec!["Zed Industries".to_owned()],
                    repository: "https://github.com/zed-industries/zed".to_owned(),
                    schema_version: Some(1),
                    wasm_api_version: None,
                    provides: BTreeSet::from_iter(provides),
                },
                published_at: Default::default(),
                download_count,
            }
        }

        fn dev_extension() -> Arc<ExtensionManifest> {
            Arc::new(ExtensionManifest {
                // Fake ID which will never be accepted during publishing
                id: "zed-demo-theme".into(),
                name: "Local Theme".to_owned(),
                version: "0.1.0".into(),
                schema_version: SchemaVersion::ZERO,
                description: Some("A locally installed extension under development.".to_owned()),
                repository: Some("https://github.com/zed-industries/zed".to_owned()),
                authors: vec!["Extension Developer".to_owned()],
                lib: Default::default(),
                themes: Vec::new(),
                icon_themes: Vec::new(),
                languages: Vec::new(),
                grammars: Default::default(),
                language_servers: Default::default(),
                context_servers: Default::default(),
                slash_commands: Default::default(),
                snippets: None,
                capabilities: Vec::new(),
                debug_adapters: Default::default(),
                debug_locators: Default::default(),
                language_model_providers: Default::default(),
                agent_tools: Default::default(),
            })
        }

        let examples = vec![
            single_example(
                "Available to Install",
                ExtensionCard::remote(
                    &remote_extension(
                        "preview-toml",
                        "TOML",
                        "0.6.2",
                        "TOML language support.",
                        482_391,
                        [ExtensionProvides::Languages],
                    ),
                    PublishedExtensionState::Store(ExtensionStatus::NotInstalled),
                    cx,
                )
                .into_any_element(),
            ),
            single_example(
                "Installed",
                ExtensionCard::remote(
                    &remote_extension(
                        "preview-python",
                        "Python",
                        "0.5.1",
                        "Python language support powered by basedpyright.",
                        1_284_613,
                        [
                            ExtensionProvides::Languages,
                            ExtensionProvides::LanguageServers,
                            ExtensionProvides::ContextServers,
                        ],
                    ),
                    PublishedExtensionState::Store(ExtensionStatus::Installed("0.5.1".into())),
                    cx,
                )
                .into_any_element(),
            ),
            single_example(
                "Update Available",
                ExtensionCard::remote(
                    &remote_extension(
                        "preview-rust",
                        "Rust",
                        "0.4.0",
                        "Rust language support powered by rust-analyzer.",
                        2_947_028,
                        [
                            ExtensionProvides::Languages,
                            ExtensionProvides::LanguageServers,
                        ],
                    ),
                    PublishedExtensionState::Store(ExtensionStatus::Installed("0.3.1".into())),
                    cx,
                )
                .into_any_element(),
            ),
            single_example(
                "Development Extension",
                ExtensionCard::manifest(
                    dev_extension(),
                    ExtensionStatus::Installed("0.1.0".into()),
                    LocalExtensionKind::Dev,
                )
                .into_any_element(),
            ),
            single_example(
                "Overridden by Development Extension",
                ExtensionCard::remote(
                    &remote_extension(
                        "preview-overridden-theme",
                        "Local Theme",
                        "1.3.0",
                        "The published version of a locally developed extension.",
                        36_512,
                        [ExtensionProvides::Themes],
                    ),
                    PublishedExtensionState::OverriddenByDevExtension,
                    cx,
                )
                .into_any_element(),
            ),
        ];

        div()
            .w_128()
            .child(example_group(examples).vertical())
            .into_any_element()
    }
}

impl RenderOnce for ExtensionCard {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            details,
            actions,
            context_menu,
        } = self;
        let ExtensionCardDetails {
            id,
            name,
            version,
            description,
            authors,
            repository_url,
            repository_icon,
            provided_features,
            source,
        } = details;
        let installed_version = source.installed_version(&version);
        let download_count = source.download_count();
        let is_dev = source.is_dev();
        let is_overridden = source.is_overridden();
        let muted = cx.theme().colors().text_muted;

        div().w_full().pt_3().child(
            v_flex()
                .relative()
                .w_full()
                .min_w_0()
                .h(rems(7.5))
                .p_3()
                .gap_2()
                .bg(cx.theme().colors().elevated_surface_background.opacity(0.5))
                .border_1()
                .border_color(cx.theme().colors().border_variant)
                .rounded_md()
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .gap_2()
                        .justify_between()
                        .child(
                            h_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(Label::new(name).text_base().truncate())
                                .child(
                                    Label::new(if is_dev {
                                        format!("v{version} (dev)")
                                    } else {
                                        format!("v{version}")
                                    })
                                    .text_sm()
                                    .text_color(muted)
                                    .flex_none(),
                                )
                                .when_some(installed_version, |this, version| {
                                    this.child(
                                        Label::new(format!("(v{version} installed)"))
                                            .text_sm()
                                            .text_color(muted)
                                            .flex_none(),
                                    )
                                })
                                .child(h_flex().min_w_0().gap_1().overflow_hidden().children(
                                    provided_features.into_iter().map(|feature| {
                                        Tag::secondary().xsmall().flex_none().child(feature)
                                    }),
                                )),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .children(actions.into_iter().flatten()),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_1()
                        .gap_2()
                        .justify_between()
                        .child(
                            Label::new(description.unwrap_or_default())
                                .min_w_0()
                                .flex_1()
                                .truncate()
                                .text_sm(),
                        )
                        .when_some(download_count, |this, count| {
                            this.child(
                                Label::new(format!(
                                    "Downloads: {}",
                                    format_number_with_commas(u128::from(count))
                                ))
                                .text_sm()
                                .text_color(muted)
                                .flex_none(),
                            )
                        }),
                )
                .child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .flex_none()
                        .gap_2()
                        .justify_between()
                        .child(
                            h_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_1()
                                .child(
                                    KitIcon::empty()
                                        .path(IconName::Person.path())
                                        .size(rems(0.875))
                                        .text_color(muted),
                                )
                                .child(
                                    Label::new(authors.clone())
                                        .min_w_0()
                                        .truncate()
                                        .text_sm()
                                        .text_color(muted),
                                ),
                        )
                        .child(
                            h_flex()
                                .flex_none()
                                .gap_1()
                                .when_some(repository_url, |this, url| {
                                    this.child(
                                        Button::new(SharedString::from(format!("repository-{id}")))
                                            .ghost()
                                            .small()
                                            .size_6()
                                            .p_0()
                                            .tooltip("Visit Extension Repository")
                                            .disabled(is_overridden)
                                            .child(
                                                MorphIcon::empty()
                                                    .id("extension-repository-icon")
                                                    .path(repository_icon.path())
                                                    .size(rems(1.125)),
                                            )
                                            .on_click(move |_, _, cx| cx.open_url(&url)),
                                    )
                                })
                                .when_some(context_menu, |this, menu_builder| {
                                    this.child(
                                        Button::new(SharedString::from(format!("more-{id}")))
                                            .ghost()
                                            .small()
                                            .icon(KitIconName::Ellipsis)
                                            .tooltip("Extension actions")
                                            .disabled(is_overridden)
                                            .dropdown_menu_with_anchor(
                                                Anchor::TopRight,
                                                move |menu, window, cx| {
                                                    menu_builder(
                                                        menu,
                                                        id.clone(),
                                                        authors.clone(),
                                                        window,
                                                        cx,
                                                    )
                                                },
                                            ),
                                    )
                                }),
                        ),
                )
                .when(is_overridden, |this| {
                    this.child(
                        h_flex()
                            .absolute()
                            .inset_0()
                            .items_center()
                            .justify_center()
                            .bg(cx.theme().colors().elevated_surface_background.alpha(0.9))
                            .child(Label::new("Overridden by dev extension.")),
                    )
                }),
        )
    }
}
