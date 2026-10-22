use crate::TerminalView;
use anyhow::{Result, anyhow};
use collections::HashMap;
use futures::{channel::oneshot, future::join_all};
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, Task, TaskExt as _, WeakEntity, Window,
};
use itertools::Itertools;
use project::Project;
use std::process::ExitStatus;
use task::{RevealStrategy, Shell, ShellBuilder, SpawnInTerminal, TaskId};
use terminal::Terminal;
use workspace::{Pane, Workspace};

pub fn init(cx: &mut App) {
    cx.observe_new(
        |workspace: &mut Workspace, _window, cx: &mut Context<Workspace>| {
            let terminals = cx.new(|_| TerminalWorkspace {
                workspace: workspace.weak_handle(),
                deferred_tasks: HashMap::default(),
            });
            workspace.set_terminal_provider(TerminalProvider(terminals));
            workspace.register_action(TerminalWorkspace::new_terminal);
            workspace.register_action(TerminalWorkspace::open_terminal);
            workspace.register_action(|workspace, _: &crate::Focus, window, cx| {
                if let Some(terminal) = workspace.recent_active_item_by_type::<TerminalView>(cx) {
                    workspace.activate_item(&terminal, true, true, window, cx);
                } else {
                    TerminalWorkspace::new_terminal(
                        workspace,
                        &workspace::NewTerminal::default(),
                        window,
                        cx,
                    );
                }
            });
        },
    )
    .detach();
}

/// Owns task reuse and queued runs for central terminal tabs.
pub struct TerminalWorkspace {
    workspace: WeakEntity<Workspace>,
    deferred_tasks: HashMap<TaskId, Task<()>>,
}

impl TerminalWorkspace {
    pub fn open_terminal(
        workspace: &mut Workspace,
        action: &workspace::OpenTerminal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let local = action.local;
        let working_directory = action.working_directory.clone();
        Self::add_center_terminal(
            workspace,
            window,
            cx,
            RevealStrategy::Always,
            move |project, cx| {
                if local {
                    project.create_local_terminal(cx)
                } else {
                    project.create_terminal_shell(Some(working_directory), cx)
                }
            },
        )
        .detach_and_log_err(cx);
    }

    /// Create a new Terminal in the current working directory or the user's home directory
    fn new_terminal(
        workspace: &mut Workspace,
        action: &workspace::NewTerminal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        TerminalView::deploy(
            workspace,
            &workspace::NewCenterTerminal {
                local: action.local,
            },
            window,
            cx,
        );
    }

    pub fn spawn_task(
        &mut self,
        task: &SpawnInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Task::ready(Err(anyhow!("failed to read workspace")));
        };

        let project = workspace.read(cx).project().read(cx);

        if project.is_via_collab() {
            return Task::ready(Err(anyhow!("cannot spawn tasks as a guest")));
        }

        let remote_client = project.remote_client();
        let is_windows = project.path_style(cx).is_windows();
        let remote_shell = remote_client
            .as_ref()
            .and_then(|remote_client| remote_client.read(cx).shell());

        let shell = if let Some(remote_shell) = remote_shell
            && task.shell == Shell::System
        {
            Shell::Program(remote_shell)
        } else {
            task.shell.clone()
        };

        let task = prepare_task_for_spawn(task, &shell, is_windows);

        if task.allow_concurrent_runs && task.use_new_terminal {
            return self.spawn_in_new_terminal(task, window, cx);
        }

        let mut terminals_for_task = self.terminals_for_task(&task.full_label, cx);
        let Some(existing) = terminals_for_task.pop() else {
            return self.spawn_in_new_terminal(task, window, cx);
        };

        let (existing_item_index, task_pane, existing_terminal) = existing;
        if task.allow_concurrent_runs {
            return self.replace_terminal(
                task,
                task_pane,
                existing_item_index,
                existing_terminal,
                window,
                cx,
            );
        }

        terminals_for_task.push((
            existing_item_index,
            task_pane.clone(),
            existing_terminal.clone(),
        ));
        let (tx, rx) = oneshot::channel();

        self.deferred_tasks.insert(
            task.id.clone(),
            cx.spawn_in(window, async move |terminals, cx| {
                wait_for_terminals_tasks(terminals_for_task, cx).await;
                let task = terminals.update_in(cx, |terminals, window, cx| {
                    if task.use_new_terminal {
                        terminals.spawn_in_new_terminal(task, window, cx)
                    } else {
                        terminals.replace_terminal(
                            task,
                            task_pane,
                            existing_item_index,
                            existing_terminal,
                            window,
                            cx,
                        )
                    }
                });
                if let Ok(task) = task {
                    tx.send(task.await).ok();
                }
            }),
        );

        cx.spawn(async move |_, _| rx.await?)
    }

    fn spawn_in_new_terminal(
        &mut self,
        task: SpawnInTerminal,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        self.workspace
            .update(cx, |workspace, cx| {
                Self::add_center_terminal(workspace, window, cx, task.reveal, |project, cx| {
                    project.create_terminal_task(task, cx)
                })
            })
            .unwrap_or_else(|error| Task::ready(Err(error)))
    }

    /// Starts a standalone interactive task, such as an external agent login.
    pub fn spawn_interactive_task(
        workspace: &mut Workspace,
        task: &SpawnInTerminal,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let project = workspace.project().read(cx);
        let remote_shell = project
            .remote_client()
            .as_ref()
            .and_then(|client| client.read(cx).shell());
        let shell = match (&task.shell, remote_shell) {
            (Shell::System, Some(shell)) => Shell::Program(shell),
            _ => task.shell.clone(),
        };
        let task = prepare_task_for_spawn(task, &shell, project.path_style(cx).is_windows());
        Self::add_center_terminal(workspace, window, cx, task.reveal, |project, cx| {
            project.create_terminal_task(task, cx)
        })
    }
    fn terminals_for_task(
        &self,
        label: &str,
        cx: &mut App,
    ) -> Vec<(usize, Entity<Pane>, Entity<TerminalView>)> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };

        let pane_terminal_views = |pane: Entity<Pane>| {
            pane.read(cx)
                .items()
                .enumerate()
                .filter_map(|(index, item)| Some((index, item.act_as::<TerminalView>(cx)?)))
                .filter_map(|(index, terminal_view)| {
                    let task_state = terminal_view.read(cx).terminal().read(cx).task()?;
                    if &task_state.spawned_task.full_label == label {
                        Some((index, terminal_view))
                    } else {
                        None
                    }
                })
                .map(move |(index, terminal_view)| (index, pane.clone(), terminal_view))
        };

        workspace
            .read(cx)
            .panes()
            .iter()
            .cloned()
            .flat_map(pane_terminal_views)
            .sorted_by_key(|(_, _, terminal_view)| terminal_view.entity_id())
            .collect()
    }

    pub fn add_center_terminal(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        reveal: RevealStrategy,
        create_terminal: impl FnOnce(
            &mut Project,
            &mut Context<Project>,
        ) -> Task<Result<Entity<Terminal>>>
        + 'static,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        if !is_enabled_in_workspace(workspace, cx) {
            return Task::ready(Err(anyhow!(
                "terminal not yet supported for remote projects"
            )));
        }
        let project = workspace.project().downgrade();
        cx.spawn_in(window, async move |workspace, cx| {
            let terminal = match project.update(cx, create_terminal)?.await {
                Ok(terminal) => terminal,
                Err(error) => {
                    workspace.update(cx, |workspace, cx| {
                        workspace.show_error(format!("Failed to start terminal: {error}"), cx);
                    })?;
                    return Err(error);
                }
            };

            workspace.update_in(cx, |workspace, window, cx| {
                let terminal_view = cx.new(|cx| {
                    TerminalView::new(
                        terminal.clone(),
                        workspace.weak_handle(),
                        workspace.database_id(),
                        workspace.project().downgrade(),
                        window,
                        cx,
                    )
                });
                // Don't steal focus from an open modal (e.g. the command palette):
                // a background terminal can finish starting up after the user has
                // moved on, and focusing it would dismiss whatever they opened.
                let focus_item =
                    reveal == RevealStrategy::Always && !workspace.has_active_modal(window, cx);
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item_inner(
                        Box::new(terminal_view),
                        reveal != RevealStrategy::Never,
                        focus_item,
                        reveal != RevealStrategy::Never,
                        None,
                        window,
                        cx,
                    );
                });
            })?;
            Ok(terminal.downgrade())
        })
    }

    fn replace_terminal(
        &self,
        task: SpawnInTerminal,
        pane: Entity<Pane>,
        _index: usize,
        view: Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Result<WeakEntity<Terminal>>> {
        let workspace = self.workspace.clone();
        let reveal = task.reveal;
        cx.spawn_in(window, async move |_, cx| {
            let project = workspace.read_with(cx, |workspace, _| workspace.project().clone())?;
            let terminal = project
                .update(cx, |project, cx| project.create_terminal_task(task, cx))
                .await?;
            view.update_in(cx, |view, window, cx| {
                view.set_terminal(terminal.clone(), window, cx)
            })?;
            workspace.update_in(cx, |workspace, window, cx| {
                let focus =
                    reveal == RevealStrategy::Always && !workspace.has_active_modal(window, cx);
                if reveal != RevealStrategy::Never {
                    pane.update(cx, |pane, cx| {
                        if let Some(index) = pane.index_for_item(&view) {
                            pane.activate_item(index, true, focus, window, cx);
                        }
                    });
                }
            })?;
            Ok(terminal.downgrade())
        })
    }
}

pub fn prepare_task_for_spawn(
    task: &SpawnInTerminal,
    shell: &Shell,
    is_windows: bool,
) -> SpawnInTerminal {
    let builder = ShellBuilder::new(shell, is_windows);
    let command_label = builder.command_label(task.command.as_deref().unwrap_or(""));
    let (command, args) = builder.build_no_quote(task.command.clone(), &task.args);

    SpawnInTerminal {
        command_label,
        command: Some(command),
        args,
        ..task.clone()
    }
}

fn is_enabled_in_workspace(workspace: &Workspace, cx: &App) -> bool {
    workspace.project().read(cx).supports_terminal(cx)
}

async fn wait_for_terminals_tasks(
    terminals_for_task: Vec<(usize, Entity<Pane>, Entity<TerminalView>)>,
    cx: &mut AsyncApp,
) {
    let pending_tasks = terminals_for_task.iter().map(|(_, _, terminal)| {
        terminal.update(cx, |terminal_view, cx| {
            terminal_view
                .terminal()
                .update(cx, |terminal, cx| terminal.wait_for_completed_task(cx))
        })
    });
    join_all(pending_tasks).await;
}

struct TerminalProvider(Entity<TerminalWorkspace>);

impl workspace::TerminalProvider for TerminalProvider {
    fn quick_actions(&self, cx: &mut App) -> Vec<(gpui::SharedString, Box<dyn gpui::Action>)> {
        crate::quick_actions::menu_actions(cx)
    }

    fn spawn(
        &self,
        task: SpawnInTerminal,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Option<Result<ExitStatus>>> {
        let terminals = self.0.clone();
        window.spawn(cx, async move |cx| {
            let terminal = terminals
                .update_in(cx, |terminals, window, cx| {
                    terminals.spawn_task(&task, window, cx)
                })
                .ok()?
                .await;
            match terminal {
                Ok(terminal) => {
                    let exit_status = terminal
                        .read_with(cx, |terminal, cx| terminal.wait_for_completed_task(cx))
                        .ok()?
                        .await?;
                    Some(Ok(exit_status))
                }
                Err(e) => Some(Err(e)),
            }
        })
    }
}
