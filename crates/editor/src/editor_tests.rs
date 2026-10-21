use super::*;
use crate::test::{build_editor, editor_lsp_test_context::EditorLspTestContext};
use futures::StreamExt;
use gpui::{TestAppContext, UpdateGlobal};
use multi_buffer::{MultiBuffer, MultiBufferOffset};
use pretty_assertions::assert_eq;
use settings::{AllLanguageSettingsContent, EditorSettingsContent, SettingsContent, SettingsStore};
use std::future::Future;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, atomic};
use util::test::{TextRangeMarker, marked_text_ranges_by};

#[cfg(any(test, feature = "test-support"))]
pub mod property_test;

/// Handle completion request passing a marked string specifying where the completion
/// should be triggered from using '|' character, what range should be replaced, and what completions
/// should be returned using '<' and '>' to delimit the range.
///
/// Also see `handle_completion_request_with_insert_and_replace`.
#[track_caller]
pub fn handle_completion_request(
    marked_string: &str,
    completions: Vec<&'static str>,
    is_incomplete: bool,
    counter: Arc<AtomicUsize>,
    cx: &mut EditorLspTestContext,
) -> impl Future<Output = ()> {
    let complete_from_marker: TextRangeMarker = '|'.into();
    let replace_range_marker: TextRangeMarker = ('<', '>').into();
    let (_, mut marked_ranges) = marked_text_ranges_by(
        marked_string,
        vec![complete_from_marker.clone(), replace_range_marker.clone()],
    );

    let complete_from_position = cx.to_lsp(MultiBufferOffset(
        marked_ranges.remove(&complete_from_marker).unwrap()[0].start,
    ));
    let range = marked_ranges.remove(&replace_range_marker).unwrap()[0].clone();
    let replace_range =
        cx.to_lsp_range(MultiBufferOffset(range.start)..MultiBufferOffset(range.end));

    let mut request =
        cx.set_request_handler::<lsp::request::Completion, _, _>(move |url, params, _| {
            let completions = completions.clone();
            counter.fetch_add(1, atomic::Ordering::Release);
            async move {
                assert_eq!(params.text_document_position.text_document.uri, url.clone());
                assert_eq!(
                    params.text_document_position.position,
                    complete_from_position
                );
                Ok(Some(lsp::CompletionResponse::List(lsp::CompletionList {
                    is_incomplete,
                    item_defaults: None,
                    items: completions
                        .iter()
                        .map(|completion_text| lsp::CompletionItem {
                            label: completion_text.to_string(),
                            text_edit: Some(lsp::CompletionTextEdit::Edit(lsp::TextEdit {
                                range: replace_range,
                                new_text: completion_text.to_string(),
                            })),
                            ..Default::default()
                        })
                        .collect(),
                })))
            }
        });

    async move {
        request.next().await;
    }
}

pub(crate) fn update_test_language_settings(
    cx: &mut TestAppContext,
    f: &dyn Fn(&mut AllLanguageSettingsContent),
) {
    cx.update(|cx| {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, &|settings: &mut SettingsContent| {
                f(&mut settings.project.all_languages)
            });
        });
    });
}

pub(crate) fn update_test_editor_settings(
    cx: &mut TestAppContext,
    f: &dyn Fn(&mut EditorSettingsContent),
) {
    cx.update(|cx| {
        SettingsStore::update_global(cx, |store, cx| {
            store.update_user_settings(cx, |settings| f(&mut settings.editor));
        })
    })
}

pub(crate) fn init_test(cx: &mut TestAppContext, f: fn(&mut AllLanguageSettingsContent)) {
    cx.update(|cx| {
        assets::Assets.load_test_fonts(cx);
        let store = SettingsStore::test(cx);
        cx.set_global(store);
        theme_settings::init(theme::LoadThemes::JustBase, cx);
        release_channel::init(semver::Version::new(0, 0, 0), cx);
        crate::init(cx);
    });
    zlog::init_test();
    update_test_language_settings(cx, &f);
}
