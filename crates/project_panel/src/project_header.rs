use std::path::Path;
use util::ResultExt as _;

use git_ui_core::worktree_picker::WorktreePicker;
use gpui::{
    Anchor, AnyElement, Context, Entity, Focusable as _, IntoElement, ParentElement, SharedString,
};
use gpui_icons::Icon as MorphIcon;
use gpui_kit::component::{
    Disableable as _, Icon as KitIcon, Sizable,
    button::{Button as KitButton, ButtonVariants},
    popover::Popover,
};
use project::{
    git_store::Repository, linked_worktree_short_name, project_settings::ProjectSettings,
    repo_identity_path, repo_identity_path_if_local,
};
use settings::Settings as _;
use ui::prelude::*;
use workspace::panel_header;

use crate::ProjectPanel;

const MAX_PROJECT_NAME_LENGTH: usize = 40;

fn linked_worktree_name_anchor<'a>(
    main_worktree_path: Option<&'a Path>,
    repository_identity_path: Option<&'a Path>,
    is_linked_worktree: bool,
) -> Option<&'a Path> {
    main_worktree_path.or_else(|| {
        is_linked_worktree
            .then_some(repository_identity_path)
            .flatten()
    })
}

impl ProjectPanel {
    /// The project's name and its worktree, each opening a picker to switch it, and the project
    /// search.
    pub(crate) fn render_project_header(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let worktree = self.effective_active_worktree(cx)?;
        let worktree_abs_path = worktree.read(cx).abs_path();
        let mut project_name = worktree
            .read(cx)
            .root_name()
            .file_name()
            .map(|name| SharedString::from(name.to_string()));
        let repository = self.repository_for_worktree(&worktree_abs_path, cx);
        let mut linked_worktree_name = None;
        if let Some(repository) = &repository {
            let repository = repository.read(cx);
            let identity =
                repo_identity_path(&repository.common_dir_abs_path, repository.path_style);
            let identity_fallback =
                repo_identity_path_if_local(&repository.common_dir_abs_path, repository.path_style);
            linked_worktree_name = linked_worktree_name_anchor(
                repository.main_worktree_abs_path(),
                identity_fallback,
                repository.is_linked_worktree(),
            )
            .and_then(|name_anchor_path| {
                linked_worktree_short_name(
                    name_anchor_path,
                    repository.work_directory_abs_path.as_ref(),
                    repository.path_style,
                )
            })
            .or_else(|| {
                repository
                    .is_linked_worktree()
                    .then_some(project_name.clone())
                    .flatten()
            });

            let display_name = if identity.extension() == Some(std::ffi::OsStr::new("git")) {
                identity.file_stem()
            } else {
                repository.path_style.file_name(identity)
            };
            if let Some(repository_name) = display_name.and_then(|name| name.to_str()) {
                let relative_path = worktree_abs_path
                    .strip_prefix(&*repository.work_directory_abs_path)
                    .ok()
                    .filter(|relative| !relative.as_os_str().is_empty());
                let name = match relative_path {
                    Some(relative) if self.visible_worktrees_in_repository(repository, cx) == 1 => {
                        format!("{}/{}", repository_name, relative.display())
                    }
                    _ => repository_name.to_string(),
                };
                project_name = Some(SharedString::from(name));
            }
        }

        let is_git_enabled = ProjectSettings::get_global(cx).git.enabled.status;
        Some(
            panel_header()
                .child(self.render_project_name(project_name))
                .children(
                    repository
                        .filter(|_| is_git_enabled)
                        .map(|_| self.render_worktree_picker(linked_worktree_name, cx)),
                ),
        )
    }

    fn render_project_name(&self, name: Option<SharedString>) -> AnyElement {
        let workspace = self.workspace.clone();
        let display_name = match &name {
            Some(name) => util::truncate_and_trailoff(name, MAX_PROJECT_NAME_LENGTH),
            None => "Open Recent Project".to_string(),
        };
        Popover::new("recent-projects-menu")
            .content(move |_, window, cx| {
                let popover = cx.weak_entity();
                let state = window.use_keyed_state("recent-projects-content", cx, |window, cx| {
                    let picker = cx.new(|cx| {
                        recent_projects::sidebar_recent_projects::SidebarRecentProjects::new(
                            workspace.clone(),
                            window,
                            cx,
                        )
                    });
                    let subscription = cx.subscribe_in(
                        &picker,
                        window,
                        move |_, _, _: &gpui::DismissEvent, window, cx| {
                            popover
                                .update(cx, |popover, cx| popover.dismiss(window, cx))
                                .log_err();
                        },
                    );
                    (picker, subscription)
                });
                state.read(cx).0.clone()
            })
            .trigger(
                KitButton::new("project-name-trigger")
                    .ghost()
                    .small()
                    .label(display_name)
                    .icon(KitIcon::empty().path(IconName::ChevronDown.path()))
                    .tooltip("Recent Projects"),
            )
            .anchor(Anchor::TopLeft)
            .into_any_element()
    }

    fn render_worktree_picker(
        &self,
        linked_worktree_name: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let worktree_label: SharedString = linked_worktree_name.unwrap_or_else(|| "main".into());
        let (creation_in_progress, is_switch) = self
            .workspace
            .upgrade()
            .map(|workspace| {
                let creation = workspace.read(cx).active_worktree_creation();
                (creation.label.clone(), creation.is_switch)
            })
            .unwrap_or((None, false));
        let display_label: SharedString = match &creation_in_progress {
            Some(name) if is_switch => format!("Loading {}…", name).into(),
            Some(name) => format!("Creating {}…", name).into(),
            None => worktree_label.clone(),
        };

        let project = self.project.clone();
        let workspace = self.workspace.clone();
        Popover::new("worktree-picker-menu")
            .content(move |_, window, cx| {
                let popover = cx.weak_entity();
                let state = window.use_keyed_state("worktree-picker-content", cx, |window, cx| {
                    let picker = cx.new(|cx| {
                        WorktreePicker::new(project.clone(), workspace.clone(), window, cx)
                    });
                    picker.focus_handle(cx).focus(window, cx);
                    let subscription = cx.subscribe_in(
                        &picker,
                        window,
                        move |_, _, _: &gpui::DismissEvent, window, cx| {
                            popover
                                .update(cx, |popover, cx| popover.dismiss(window, cx))
                                .log_err();
                        },
                    );
                    (picker, subscription)
                });
                state.read(cx).0.clone()
            })
            .trigger(
                KitButton::new("worktree-picker-trigger")
                    .ghost()
                    .small()
                    .child(
                        MorphIcon::empty()
                            .id("worktree-picker-icon")
                            .path(if creation_in_progress.is_some() {
                                IconName::LoadCircle.path()
                            } else {
                                IconName::GitWorktree.path()
                            })
                            .size_3p5()
                            .spinning(creation_in_progress.is_some()),
                    )
                    .child(display_label)
                    .disabled(creation_in_progress.is_some())
                    .tooltip(format!("Worktree: {worktree_label}")),
            )
            .anchor(Anchor::TopLeft)
            .into_any_element()
    }

    /// The worktree the header names: the one holding the project's active repository, or else
    /// the first visible one.
    fn effective_active_worktree(&self, cx: &App) -> Option<Entity<project::Worktree>> {
        let project = self.project.read(cx);
        if let Some(repository) = project.active_repository(cx) {
            let repository_path = &repository.read(cx).work_directory_abs_path;
            for worktree in project.visible_worktrees(cx) {
                let worktree_path = worktree.read(cx).abs_path();
                if worktree_path == *repository_path
                    || worktree_path.starts_with(repository_path.as_ref())
                {
                    return Some(worktree);
                }
            }
        }
        project.visible_worktrees(cx).next()
    }

    fn repository_for_worktree(
        &self,
        worktree_path: &Path,
        cx: &App,
    ) -> Option<Entity<Repository>> {
        self.project
            .read(cx)
            .git_store()
            .read(cx)
            .repositories()
            .values()
            .filter(|repository| {
                let repository_path = &repository.read(cx).work_directory_abs_path;
                worktree_path == repository_path.as_ref()
                    || worktree_path.starts_with(repository_path.as_ref())
            })
            .max_by_key(|repository| {
                repository
                    .read(cx)
                    .work_directory_abs_path
                    .as_os_str()
                    .len()
            })
            .cloned()
    }

    fn visible_worktrees_in_repository(&self, repository: &Repository, cx: &App) -> usize {
        let repository_path = &repository.work_directory_abs_path;
        self.project
            .read(cx)
            .visible_worktrees(cx)
            .filter(|worktree| {
                let worktree_path = worktree.read(cx).abs_path();
                worktree_path == *repository_path
                    || worktree_path.starts_with(repository_path.as_ref())
            })
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use util::paths::PathStyle;

    #[test]
    fn test_foreign_path_style_does_not_use_repository_identity_as_name_anchor() {
        let (common_dir, foreign_path_style) = match PathStyle::local() {
            PathStyle::Unix => (Path::new(r"C:\repos\zed"), PathStyle::Windows),
            PathStyle::Windows => (Path::new("/repos/zed"), PathStyle::Unix),
        };
        let repository_identity_path = repo_identity_path_if_local(common_dir, foreign_path_style);

        assert_eq!(
            linked_worktree_name_anchor(None, repository_identity_path, true),
            None
        );
    }

    #[test]
    fn test_local_path_style_uses_bare_repository_worktree_name() {
        let (repository_identity_path, work_directory_path) = match PathStyle::local() {
            PathStyle::Unix => (
                Path::new("/repos/zed"),
                Path::new("/worktrees/zed/plum-warbler/zed"),
            ),
            PathStyle::Windows => (
                Path::new(r"C:\repos\zed"),
                Path::new(r"C:\worktrees\zed\plum-warbler\zed"),
            ),
        };

        let repository_identity_path =
            repo_identity_path_if_local(repository_identity_path, PathStyle::local());
        let name_anchor_path = linked_worktree_name_anchor(None, repository_identity_path, true);

        assert_eq!(
            name_anchor_path.and_then(|name_anchor_path| {
                linked_worktree_short_name(
                    name_anchor_path,
                    work_directory_path,
                    PathStyle::local(),
                )
            }),
            Some("plum-warbler".into())
        );
    }
}
