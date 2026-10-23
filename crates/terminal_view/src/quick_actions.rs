use anyhow::{Result, anyhow, bail, ensure};
use db::kvp::KeyValueStore;
use gpui::{
    Action, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Global,
    InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Subscription, Task, WeakEntity, Window, div,
    prelude::FluentBuilder as _, px,
};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputState, Textarea, TextareaState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use terminal::CommandInputState;
use util::ResultExt as _;
use workspace::{Item, Workspace, item::TabCategory};

use crate::{TerminalView, default_working_directory, terminal_workspace::TerminalWorkspace};

const STORAGE_KEY: &str = "terminal_quick_actions";

#[derive(Clone, Serialize, Deserialize)]
struct QuickAction {
    id: String,
    name: String,
    steps: Vec<CommandStep>,
}

#[derive(Clone, Serialize, Deserialize)]
struct CommandStep {
    command: String,
    target: Target,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Target {
    Current,
    New,
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Self::Current => "Current terminal",
            Self::New => "New terminal",
        }
    }
}

impl QuickAction {
    fn validate(&self) -> Result<()> {
        ensure!(!self.name.trim().is_empty(), "Enter an action name.");
        ensure!(!self.steps.is_empty(), "Add at least one command.");
        for (index, step) in self.steps.iter().enumerate() {
            ensure!(
                !step.command.trim().is_empty(),
                "Command {} is empty.",
                index + 1
            );
            ensure!(
                !step.command.contains('\0'),
                "Command {} contains a NUL character.",
                index + 1
            );
        }
        Ok(())
    }
}

#[derive(Clone, Default, PartialEq, Deserialize, JsonSchema, Action)]
#[action(namespace = terminal)]
struct RunQuickAction {
    id: String,
}

struct Store {
    actions: Vec<QuickAction>,
    load_error: Option<String>,
    saving: bool,
    running: Option<String>,
    execution: Option<Task<()>>,
}

struct GlobalStore(Entity<Store>);
impl Global for GlobalStore {}

fn store(cx: &mut App) -> Entity<Store> {
    if let Some(store) = cx.try_global::<GlobalStore>() {
        return store.0.clone();
    }
    let loaded = (|| -> Result<Vec<QuickAction>> {
        let saved = KeyValueStore::global(cx).read_kvp(STORAGE_KEY)?;
        Ok(saved
            .map(|saved| serde_json::from_str(&saved))
            .transpose()?
            .unwrap_or_default())
    })();
    let (actions, load_error) = match loaded {
        Ok(actions) => (actions, None),
        Err(error) => {
            log::error!("Failed to load terminal quick actions: {error:#}");
            (
                Vec::new(),
                Some(format!("Failed to load quick actions: {error:#}")),
            )
        }
    };
    let store = cx.new(|_| Store {
        actions,
        load_error,
        saving: false,
        running: None,
        execution: None,
    });
    cx.set_global(GlobalStore(store.clone()));
    store
}

pub(super) fn menu_actions(cx: &mut App) -> Vec<(SharedString, Box<dyn Action>)> {
    store(cx)
        .read(cx)
        .actions
        .iter()
        .map(|action| {
            (
                action.name.clone().into(),
                Box::new(RunQuickAction {
                    id: action.id.clone(),
                }) as Box<dyn Action>,
            )
        })
        .collect()
}

pub(super) fn init(cx: &mut App) {
    cx.observe_new(|workspace: &mut Workspace, _, _| {
        workspace.register_action(
            |workspace, _: &workspace::OpenTerminalQuickActions, window, cx| {
                if let Some(page) = workspace.panes().iter().find_map(|pane| {
                    pane.read(cx)
                        .items()
                        .find_map(|item| item.downcast::<QuickActionsPage>())
                }) {
                    workspace.activate_item(&page, true, true, window, cx);
                    return;
                }
                let store = store(cx);
                let workspace_handle = workspace.weak_handle();
                let page = cx.new(|cx| QuickActionsPage::new(store, workspace_handle, cx));
                workspace.add_item_to_active_pane(Box::new(page), None, true, window, cx);
            },
        );
        workspace.register_action(|workspace, action: &RunQuickAction, window, cx| {
            run(workspace, &action.id, window, cx);
        });
    })
    .detach();
}

fn run(workspace: &mut Workspace, id: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let store = store(cx);
    let state = store.read(cx);
    if state.running.is_some() {
        workspace.show_error(
            "A quick action is already running. Stop it from the quick actions tab first.",
            cx,
        );
        return;
    }
    let Some(action) = state.actions.iter().find(|action| action.id == id).cloned() else {
        workspace.show_error("This quick action no longer exists.", cx);
        return;
    };
    if let Err(error) = action.validate() {
        workspace.show_error(error, cx);
        return;
    }
    let initial = workspace
        .recent_active_item_by_type::<TerminalView>(cx)
        .map(|view| view.read(cx).terminal().downgrade());
    let directory = default_working_directory(workspace, cx);
    let workspace = workspace.weak_handle();
    store.update(cx, |store, cx| {
        store.running = Some(action.name.clone());
        store.execution = Some(cx.spawn_in(window, async move |store, cx| {
            let result: Result<()> = async {
                let mut active = initial;
                for step in action.steps {
                    if step.target == Target::New || active.is_none() {
                        let directory = match &active {
                            Some(terminal) => terminal.read_with(cx, |terminal, _| terminal.working_directory())?.or_else(|| directory.clone()),
                            None => directory.clone(),
                        };
                        let terminal = workspace.update_in(cx, |workspace, window, cx| {
                            TerminalWorkspace::add_center_terminal(workspace, window, cx, task::RevealStrategy::Always,
                                move |project, cx| project.create_terminal_shell(directory, cx))
                        })?.await?;
                        active = Some(terminal);
                    }
                    let terminal = active.as_ref().ok_or_else(|| anyhow!("No terminal is available."))?;
                    let mut starting_since = None;
                    loop {
                        workspace.read_with(cx, |_, _| ())?;
                        let state = terminal.read_with(cx, |terminal, _| terminal.command_input_state())?;
                        match state {
                            CommandInputState::Ready => break,
                            CommandInputState::Exited => bail!("The terminal has exited."),
                            CommandInputState::Unavailable => bail!("Quick actions require shell integration. Open a terminal using a supported shell."),
                            CommandInputState::Starting => {
                                let started = starting_since.get_or_insert_with(Instant::now);
                                ensure!(started.elapsed() <= Duration::from_secs(30), "Timed out waiting for the terminal to become ready.");
                            }
                            CommandInputState::Running => starting_since = None,
                        }
                        cx.background_executor().timer(Duration::from_millis(50)).await;
                    }
                    terminal.update(cx, |terminal, cx| terminal.submit_command(step.command, cx))??;
                    workspace.update_in(cx, |workspace, window, cx| {
                        let view = workspace.panes().iter().find_map(|pane| {
                            pane.read(cx).items().filter_map(|item| item.downcast::<TerminalView>())
                                .find(|view| view.read(cx).terminal().entity_id() == terminal.entity_id())
                        });
                        if let Some(view) = view {
                            workspace.activate_item(&view, true, true, window, cx);
                        }
                    })?;
                }
                Ok(())
            }.await;
            store.update(cx, |store, cx| {
                store.running = None;
                cx.notify();
            }).log_err();
            if let Err(error) = result {
                workspace.update(cx, |workspace, cx| workspace.show_error(format!("Quick action failed: {error:#}"), cx)).log_err();
            }
        }));
        cx.notify();
    });
}

struct StepDraft {
    command: Entity<TextareaState>,
    target: Target,
}

impl StepDraft {
    fn new(step: CommandStep, window: &mut Window, cx: &mut App) -> Self {
        Self {
            command: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Shell command")
                    .default_value(step.command)
            }),
            target: step.target,
        }
    }
}

struct Draft {
    id: String,
    name: Entity<InputState>,
    steps: Vec<StepDraft>,
}

struct QuickActionsPage {
    store: Entity<Store>,
    workspace: WeakEntity<Workspace>,
    focus_handle: FocusHandle,
    draft: Option<Draft>,
    error: Option<String>,
    _subscription: Subscription,
}

impl QuickActionsPage {
    fn new(store: Entity<Store>, workspace: WeakEntity<Workspace>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            workspace,
            focus_handle: cx.focus_handle(),
            draft: None,
            error: None,
            _subscription: subscription,
        }
    }

    fn edit(&mut self, action: QuickAction, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Action name")
                .default_value(action.name)
        });
        name.update(cx, |name, cx| name.focus(window, cx));
        self.draft = Some(Draft {
            id: action.id,
            name,
            steps: action
                .steps
                .into_iter()
                .map(|step| StepDraft::new(step, window, cx))
                .collect(),
        });
        self.error = None;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else {
            return;
        };
        let action = QuickAction {
            id: draft.id.clone(),
            name: draft.name.read(cx).value().trim().to_owned(),
            steps: draft
                .steps
                .iter()
                .map(|step| CommandStep {
                    command: step.command.read(cx).value().to_string(),
                    target: step.target,
                })
                .collect(),
        };
        if let Err(error) = action.validate() {
            self.error = Some(error.to_string());
            cx.notify();
            return;
        }
        let mut actions = self.store.read(cx).actions.clone();
        if let Some(existing) = actions.iter_mut().find(|existing| existing.id == action.id) {
            *existing = action;
        } else {
            actions.push(action);
        }
        self.persist(actions, cx);
    }

    fn persist(&mut self, actions: Vec<QuickAction>, cx: &mut Context<Self>) {
        if self.store.read(cx).saving || self.store.read(cx).load_error.is_some() {
            return;
        }
        let serialized = match serde_json::to_string(&actions) {
            Ok(serialized) => serialized,
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.error = None;
        self.store.update(cx, |store, cx| {
            store.saving = true;
            cx.notify();
        });
        let database = KeyValueStore::global(cx);
        let store = self.store.clone();
        let workspace = self.workspace.clone();
        cx.spawn(async move |page, cx| {
            let result = database.write_kvp(STORAGE_KEY.into(), serialized).await;
            store.update(cx, |store, cx| {
                store.saving = false;
                if result.is_ok() {
                    store.actions = actions;
                }
                cx.notify();
            });
            let error = result
                .err()
                .map(|error| format!("Failed to save quick actions: {error:#}"));
            if let Some(error) = &error {
                log::error!("{error}");
                workspace
                    .update(cx, |workspace, cx| workspace.show_error(error.clone(), cx))
                    .log_err();
            }
            if let Some(page) = page.upgrade() {
                page.update(cx, |page, cx| {
                    if error.is_none() {
                        page.draft = None;
                    }
                    page.error = error;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn render_draft(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let draft = self.draft.as_ref()?;
        let saving = self.store.read(cx).saving;
        Some(
            v_flex()
                .gap_4()
                .p_4()
                .rounded_lg()
                .bg(cx.theme().muted)
                .child(div().text_sm().child("Action name"))
                .child(
                    Input::new(&draft.name)
                        .aria_label("Action name")
                        .disabled(saving),
                )
                .children(draft.steps.iter().enumerate().map(|(index, step)| {
                    let page = cx.entity().downgrade();
                    let selected_target = step.target;
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(div().flex_1().child(format!("Command {}", index + 1)))
                                .child(
                                    Button::new(("target", index))
                                        .small()
                                        .outline()
                                        .disabled(saving)
                                        .label(step.target.label())
                                        .dropdown_menu(move |menu, window, _| {
                                            let menu = workspace::style_menu(menu, window);
                                            [Target::Current, Target::New].into_iter().fold(
                                                menu,
                                                |menu, target| {
                                                    let page = page.clone();
                                                    menu.item(
                                                        PopupMenuItem::new(target.label())
                                                            .icon(match target {
                                                                Target::Current => {
                                                                    workspace::menu_icon(gpui_icons::IconName::TerminalRectangle)
                                                                }
                                                                Target::New => {
                                                                    workspace::menu_icon(gpui_icons::IconName::Plus)
                                                                }
                                                            })
                                                            .checked(target == selected_target)
                                                            .on_click(move |_, _, cx| {
                                                                page.update(cx, |page, cx| {
                                                                    if let Some(step) = page
                                                                        .draft
                                                                        .as_mut()
                                                                        .and_then(|draft| {
                                                                            draft
                                                                                .steps
                                                                                .get_mut(index)
                                                                        })
                                                                    {
                                                                        step.target = target;
                                                                    }
                                                                    cx.notify();
                                                                })
                                                                .log_err();
                                                            }),
                                                    )
                                                },
                                            )
                                        }),
                                )
                                .child(
                                    Button::new(("remove-step", index))
                                        .small()
                                        .ghost()
                                        .label("Remove")
                                        .disabled(saving)
                                        .on_click(cx.listener(move |page, _, _, cx| {
                                            if let Some(draft) = &mut page.draft
                                                && index < draft.steps.len()
                                            {
                                                draft.steps.remove(index);
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            Textarea::new(&step.command)
                                .aria_label(format!("Command {}", index + 1))
                                .h(px(100.))
                                .disabled(saving),
                        )
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(
                            Button::new("add-command")
                                .outline()
                                .label("Add command")
                                .disabled(saving)
                                .on_click(cx.listener(|page, _, window, cx| {
                                    if let Some(draft) = &mut page.draft {
                                        draft.steps.push(StepDraft::new(
                                            CommandStep {
                                                command: String::new(),
                                                target: Target::Current,
                                            },
                                            window,
                                            cx,
                                        ));
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("cancel-edit")
                                .ghost()
                                .label("Cancel")
                                .disabled(saving)
                                .on_click(cx.listener(|page, _, window, cx| {
                                    page.draft = None;
                                    page.error = None;
                                    page.focus_handle.focus(window, cx);
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("save-action")
                                .primary()
                                .label(if saving { "Saving…" } else { "Save" })
                                .disabled(saving)
                                .on_click(cx.listener(|page, _, _, cx| page.save(cx))),
                        ),
                ),
        )
    }
}

impl EventEmitter<()> for QuickActionsPage {}
impl Focusable for QuickActionsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl Item for QuickActionsPage {
    type Event = ();
    fn tab_category(&self, _: &App) -> TabCategory {
        TabCategory::Terminal
    }
    fn tab_content_text(&self, _: usize, _: &App) -> SharedString {
        "Terminal quick actions".into()
    }
}

impl Render for QuickActionsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let store = self.store.read(cx);
        let actions = store.actions.clone();
        let saving = store.saving;
        let running = store.running.clone();
        let error = self.error.clone().or_else(|| store.load_error.clone());
        let editable = !saving && store.load_error.is_none();
        let draft_is_new = self
            .draft
            .as_ref()
            .is_some_and(|draft| !actions.iter().any(|action| action.id == draft.id));
        v_flex().id("terminal-quick-actions-page").track_focus(&self.focus_handle)
            .size_full().overflow_y_scroll().p_6().gap_4().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(v_flex().flex_none().w_full().max_w(px(800.)).mx_auto().gap_4()
                .child(div().text_xl().child("Terminal quick actions"))
                .child(div().text_sm().text_color(cx.theme().muted_foreground)
                    .child("Run saved commands in the most recent terminal, or start a new terminal for a step. Later steps targeting the same terminal wait for its shell prompt."))
                .when_some(running.clone(), |page, name| page.child(h_flex().gap_2()
                    .child(div().flex_1().child(format!("Running: {name}")))
                    .child(Button::new("stop-quick-action").outline().label("Stop remaining steps")
                        .on_click(cx.listener(|page, _, _, cx| {
                            page.store.update(cx, |store, cx| { store.execution = None; store.running = None; cx.notify(); });
                        })))))
                .when_some(error, |page, error| page.child(div().text_color(cx.theme().danger).child(error)))
                .when(actions.is_empty(), |page| page.child(div().text_sm().child("No quick actions yet. Add one below.")))
                .children(actions.into_iter().map(|action| {
                    let id = action.id.clone();
                    let edit_action = action.clone();
                    let delete_id = action.id.clone();
                    let editing = self.draft.as_ref().is_some_and(|draft| draft.id == action.id);
                    v_flex().gap_3().p_4().rounded_lg().border_1().border_color(cx.theme().border)
                        .child(h_flex().gap_2().flex_wrap()
                            .child(v_flex().flex_1().min_w_0().child(action.name)
                                .child(div().text_sm().text_color(cx.theme().muted_foreground).child(format!("{} commands", action.steps.len()))))
                            .child(Button::new(SharedString::from(format!("run-{id}"))).small().primary().label("Run").disabled(running.is_some())
                                .on_click(cx.listener(move |page, _, window, cx| {
                                    page.workspace.update(cx, |workspace, cx| run(workspace, &id, window, cx)).log_err();
                                })))
                            .child(Button::new(SharedString::from(format!("edit-{}", action.id))).small().outline().label("Edit").disabled(!editable || self.draft.is_some())
                                .on_click(cx.listener(move |page, _, window, cx| page.edit(edit_action.clone(), window, cx))))
                            .child(Button::new(SharedString::from(format!("delete-{}", action.id))).small().ghost().label("Delete").disabled(!editable || self.draft.is_some())
                                .on_click(cx.listener(move |page, _, _, cx| {
                                    let actions = page.store.read(cx).actions.iter().filter(|action| action.id != delete_id).cloned().collect();
                                    page.persist(actions, cx);
                                }))))
                        .when(editing, |card| card.children(self.render_draft(cx)))
                }))
                .when(draft_is_new, |page| page.children(self.render_draft(cx)))
                .when(self.draft.is_none(), |page| page.child(Button::new("new-quick-action").outline().label("Add quick action").disabled(!editable)
                    .on_click(cx.listener(|page, _, window, cx| page.edit(QuickAction {
                        id: uuid::Uuid::new_v4().to_string(), name: String::new(),
                        steps: vec![CommandStep { command: String::new(), target: Target::Current }],
                    }, window, cx))))))
    }
}
