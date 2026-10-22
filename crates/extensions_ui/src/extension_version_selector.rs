use std::str::FromStr;
use std::sync::Arc;

use cloud_api_types::ExtensionMetadata;
use extension_host::ExtensionStore;
use fs::Fs;
use fuzzy::{StringMatch, StringMatchCandidate, match_strings};
use gpui_kit::component::{Sizable as _, h_flex, list::ListItem, tag::Tag, v_flex};
use gpui_kit::{
    App, DismissEvent, Entity, EventEmitter, Focusable, Task, WeakEntity, Window, prelude::*,
};
use picker::{Picker, PickerDelegate};
use release_channel::ReleaseChannel;
use semver::Version;
use settings::update_settings_file;
use ui::ActiveTheme as _;
use util::ResultExt;
use workspace::ModalView;

pub struct ExtensionVersionSelector {
    picker: Entity<Picker<ExtensionVersionSelectorDelegate>>,
}

impl ModalView for ExtensionVersionSelector {}

impl EventEmitter<DismissEvent> for ExtensionVersionSelector {}

impl Focusable for ExtensionVersionSelector {
    fn focus_handle(&self, cx: &App) -> gpui_kit::FocusHandle {
        self.picker.focus_handle(cx)
    }
}

impl Render for ExtensionVersionSelector {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        v_flex().child(self.picker.clone())
    }
}

impl ExtensionVersionSelector {
    pub fn new(
        delegate: ExtensionVersionSelectorDelegate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let picker = cx.new(|cx| Picker::uniform_list(delegate, window, cx));
        Self { picker }
    }
}

pub struct ExtensionVersionSelectorDelegate {
    fs: Arc<dyn Fs>,
    selector: WeakEntity<ExtensionVersionSelector>,
    extension_versions: Vec<ExtensionMetadata>,
    selected_index: usize,
    matches: Vec<StringMatch>,
}

impl ExtensionVersionSelectorDelegate {
    pub fn new(
        fs: Arc<dyn Fs>,
        selector: WeakEntity<ExtensionVersionSelector>,
        mut extension_versions: Vec<ExtensionMetadata>,
    ) -> Self {
        extension_versions.sort_unstable_by(|a, b| {
            let a_version = Version::from_str(&a.manifest.version);
            let b_version = Version::from_str(&b.manifest.version);

            match (a_version, b_version) {
                (Ok(a_version), Ok(b_version)) => b_version.cmp(&a_version),
                _ => b.published_at.cmp(&a.published_at),
            }
        });

        let matches = extension_versions
            .iter()
            .map(|extension| StringMatch {
                candidate_id: 0,
                score: 0.0,
                positions: Default::default(),
                string: format!("v{}", extension.manifest.version),
            })
            .collect();

        Self {
            fs,
            selector,
            extension_versions,
            selected_index: 0,
            matches,
        }
    }
}

impl PickerDelegate for ExtensionVersionSelectorDelegate {
    type ListItem = ListItem;

    fn name() -> &'static str {
        "extension version selector"
    }

    fn placeholder_text(&self, _window: &mut Window, _cx: &mut App) -> Arc<str> {
        "Select extension version...".into()
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

    fn update_matches(
        &mut self,
        query: String,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Task<()> {
        let background_executor = cx.background_executor().clone();
        let candidates = self
            .extension_versions
            .iter()
            .enumerate()
            .map(|(id, extension)| {
                StringMatchCandidate::new(id, &format!("v{}", extension.manifest.version))
            })
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
                    background_executor,
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

    fn confirm(&mut self, _secondary: bool, window: &mut Window, cx: &mut Context<Picker<Self>>) {
        if self.matches.is_empty() {
            self.dismissed(window, cx);
            return;
        }

        let candidate_id = self.matches[self.selected_index].candidate_id;
        let extension_version = &self.extension_versions[candidate_id];

        if !extension_host::is_version_compatible(ReleaseChannel::global(cx), extension_version) {
            return;
        }

        let extension_store = ExtensionStore::global(cx);
        extension_store.update(cx, |store, cx| {
            let extension_id = extension_version.id.clone();
            let version = extension_version.manifest.version.clone();

            update_settings_file(self.fs.clone(), cx, {
                let extension_id = extension_id.clone();
                move |settings, _| {
                    settings
                        .extension
                        .auto_update_extensions
                        .insert(extension_id, false);
                }
            });

            store.install_extension(extension_id, version, cx);
        });
    }

    fn dismissed(&mut self, _: &mut Window, cx: &mut Context<Picker<Self>>) {
        self.selector.update(cx, |_, cx| cx.emit(DismissEvent)).ok();
    }

    fn render_match(
        &self,
        ix: usize,
        selected: bool,
        _: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> Option<Self::ListItem> {
        let version_match = &self.matches.get(ix)?;
        let extension_version = &self.extension_versions.get(version_match.candidate_id)?;

        let is_version_compatible =
            extension_host::is_version_compatible(ReleaseChannel::global(cx), extension_version);
        let disabled = !is_version_compatible;

        let text = version_match.string.clone();
        let highlights = version_match
            .positions
            .iter()
            .filter_map(|position| {
                let character = text.get(*position..)?.chars().next()?;
                Some((
                    *position..*position + character.len_utf8(),
                    gpui_kit::HighlightStyle {
                        font_weight: Some(gpui_kit::FontWeight::BOLD),
                        ..Default::default()
                    },
                ))
            })
            .collect::<Vec<_>>();
        Some(
            ListItem::new(ix)
                .py_2()
                .selected(selected)
                .disabled(disabled)
                .accessibility_label(text.clone())
                .child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .justify_between()
                        .when(disabled, |this| {
                            this.text_color(cx.theme().colors().text_muted)
                        })
                        .child(gpui_kit::StyledText::new(text).with_highlights(highlights))
                        .child(
                            h_flex()
                                .gap_2()
                                .when(disabled, |this| {
                                    this.child(Tag::secondary().small().child("Incompatible"))
                                })
                                .child(
                                    extension_version
                                        .published_at
                                        .format("%Y-%m-%d")
                                        .to_string(),
                                ),
                        ),
                ),
        )
    }
}
