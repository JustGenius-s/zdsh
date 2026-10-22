//! Agent-side glue for the [`sandbox`] crate.
//!
//! Centralizes the "should agent-run terminal commands be sandboxed for this
//! thread?" check so the system prompt, the terminal tool, and any other
//! caller see the same answer (and so the `target_os` gate lives in one
//! place instead of scattered across the agent crate).
//!
//! The current policy is: enabled iff the project is local, the platform has
//! an integration, and the thread's permissions aren't full access. Full
//! access turns sandboxing off for the model-facing surface entirely: the
//! plain (non-sandboxed) `terminal` tool is exposed and the system prompt
//! omits the sandbox section, since every command would run without a wrap
//! anyway. The model-requested `unsandboxed: true` escape approved "once" or
//! "for this thread" does NOT change the prompt/tool set — the sandboxed tool
//! stays exposed and only the individual command runs without a wrap. See
//! `sandboxing_enabled_for_project` and `ThreadSandboxGrants`.
//!
//! macOS (Seatbelt), Linux (Bubblewrap), and Windows (Bubblewrap via WSL)
//! have real sandbox integrations; on platforms without one the per-command
//! wrap is a no-op, so commands run with the agent's ambient permissions.
//!
//! Naming note: this module is about agent terminal sandboxing specifically.
//! Other agent operations (e.g. file edits) are gated separately.

use agent_settings::{AgentPermissions, SandboxPermissions};
use gpui::App;
use project::Project;
use sandbox::{HostFilesystemLocation, SandboxFsPolicy, SandboxPolicy};
use settings::GrantedWritePath;
use std::path::PathBuf;

/// How a thread's terminal commands are sandboxed, derived from its
/// [`AgentPermissions`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxMode {
    /// Sandboxed, with no write access to the project. Neither persistent nor
    /// thread grants apply.
    ReadOnly,
    /// Sandboxed, with write access to the project's worktrees.
    WorkspaceWrite,
    /// Not sandboxed.
    FullAccess,
}

impl From<AgentPermissions> for SandboxMode {
    fn from(permissions: AgentPermissions) -> Self {
        match permissions {
            AgentPermissions::ReadOnly => SandboxMode::ReadOnly,
            AgentPermissions::Workspace | AgentPermissions::AutoReview => {
                SandboxMode::WorkspaceWrite
            }
            AgentPermissions::FullAccess => SandboxMode::FullAccess,
        }
    }
}

/// Whether a thread may ask the user for access beyond what its
/// [`SandboxMode`] grants, derived from its [`AgentPermissions`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApprovalPolicy {
    /// Ask the user.
    Ask,
    /// Have a model review requests to write to specific paths, and ask the
    /// user about everything else, or when the review doesn't allow one.
    Auto,
    /// Never ask: every such request is denied without a prompt.
    Never,
}

impl From<AgentPermissions> for ApprovalPolicy {
    fn from(permissions: AgentPermissions) -> Self {
        match permissions {
            AgentPermissions::Workspace => ApprovalPolicy::Ask,
            AgentPermissions::AutoReview => ApprovalPolicy::Auto,
            AgentPermissions::ReadOnly | AgentPermissions::FullAccess => ApprovalPolicy::Never,
        }
    }
}

/// Returned to the model when [`ApprovalPolicy::Never`] denies a request that
/// would otherwise ask the user.
pub(crate) const REQUEST_NOT_ALLOWED_MESSAGE: &str = "This thread's permissions don't allow \
    asking the user for this, so it was denied. If the task needs it, ask the user to change \
    the thread's permissions.";

/// Returned to the model when a read-only thread runs a terminal command that
/// can't be sandboxed, so nothing would stop it from writing.
pub(crate) const UNSANDBOXED_READ_ONLY_MESSAGE: &str = "This command would run without a \
    sandbox, so a thread with read-only permissions can't run it. Use the read and search tools \
    instead, or ask the user to change the thread's permissions.";

/// The directory subtrees the sandbox always grants write access to for a
/// project under `mode`: its worktree roots, or nothing when read-only. This is
/// the single source of truth shared by the terminal tool (which hands these to
/// the sandbox as [`acp_thread::SandboxWrap::writable_paths`]) and the status UI
/// (which lists them), so the two can't drift if the set ever changes.
pub fn sandbox_worktree_writable_paths(
    project: &Project,
    mode: SandboxMode,
    cx: &App,
) -> Vec<PathBuf> {
    if mode == SandboxMode::ReadOnly {
        return Vec::new();
    }
    project
        .worktrees(cx)
        .map(|worktree| worktree.read(cx).abs_path().to_path_buf())
        .collect()
}

/// The candidate `.git` paths the sandbox protects for a project. Locating these
/// requires Git knowledge the sandbox layer can't derive itself: a worktree's
/// `.git`, a linked worktree's common dir (which lives outside the worktree),
/// and every discovered repository's git/common dirs.
pub fn sandbox_git_dirs(project: &Project, cx: &App) -> Vec<PathBuf> {
    let mut git_dirs = Vec::new();

    for worktree in project.worktrees(cx) {
        let worktree = worktree.read(cx);
        let worktree_abs_path = worktree.abs_path();
        // Protect `<worktree>/.git` even when it doesn't exist yet, so a command
        // can't `git init` and then write to the freshly created metadata.
        //
        // We don't gate this on the worktree's scanned root being a directory:
        // that state can be stale/pending, and for a *single-file* worktree
        // (e.g. `settings.json` opened on its own) this synthesizes
        // `settings.json/.git`, which can never exist. That impossible path is
        // resolved authoritatively at capture time — the real filesystem
        // reports `NotADirectory`, and `SandboxWrap::to_policy` skips a
        // protected path that can't exist — so it never reaches enforcement.
        git_dirs.push(worktree_abs_path.join(".git"));
        if let Some(root_repo_common_dir) = worktree.root_repo_common_dir() {
            git_dirs.push(root_repo_common_dir.to_path_buf());
        }
    }

    for repository in project.git_store().read(cx).repositories().values() {
        let repository = repository.read(cx);
        git_dirs.push(repository.dot_git_abs_path.to_path_buf());
        git_dirs.push(repository.repository_dir_abs_path.to_path_buf());
        git_dirs.push(repository.common_dir_abs_path.to_path_buf());
    }

    git_dirs.sort();
    git_dirs.dedup();
    git_dirs
}

/// What sandbox a thread applies to agent terminal commands, as one value the
/// UI renders and enforcement builds from. "No sandbox" is its own variant
/// rather than a maximally-permissive [`SandboxPolicy`] so that a wide-open but
/// real sandbox (e.g. `allow_fs_write_all`) stays
/// distinguishable from running with no sandbox at all. The sandbox still
/// enforces invariants such as read-only Git metadata, while unsandboxed commands
/// run with ambient permissions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ThreadSandbox {
    /// No OS sandbox is applied; commands run with ambient permissions.
    Unsandboxed,
    /// A sandbox is applied with this scope.
    Sandboxed(SandboxPolicy),
}

impl ThreadSandbox {
    /// Combine two layers (e.g. the persistent settings and this thread's
    /// grants).
    ///
    /// This is treated as an allowlist - i.e. merging a sandbox that allows
    /// resource A with a sandbox that allows resource B creates a sandbox with
    /// access to both resource A and resource B.
    pub fn merge(self, other: ThreadSandbox) -> ThreadSandbox {
        match (self, other) {
            (ThreadSandbox::Unsandboxed, _) | (_, ThreadSandbox::Unsandboxed) => {
                ThreadSandbox::Unsandboxed
            }
            (ThreadSandbox::Sandboxed(a), ThreadSandbox::Sandboxed(b)) => {
                ThreadSandbox::Sandboxed(a.merge(b))
            }
        }
    }

    /// Whether no OS sandbox is applied.
    pub fn is_unsandboxed(&self) -> bool {
        matches!(self, ThreadSandbox::Unsandboxed)
    }

    /// Attach the project's protected paths to a sandboxed layer. The settings
    /// and grants don't know the project's `.git` locations, so the caller
    /// computes them via [`sandbox_git_dirs`]. A no-op for `Unsandboxed`.
    pub fn with_protected_paths(self, protected_paths: Vec<PathBuf>) -> ThreadSandbox {
        match self {
            ThreadSandbox::Unsandboxed => ThreadSandbox::Unsandboxed,
            ThreadSandbox::Sandboxed(policy) => {
                // Capture each protected location (pinning its inode / canonical
                // path). A location that can't be captured is dropped — fail-closed.
                let protected_paths = protected_paths
                    .into_iter()
                    .filter_map(|path| HostFilesystemLocation::capture(path).ok())
                    .collect();
                ThreadSandbox::Sandboxed(policy.with_protected_paths(protected_paths))
            }
        }
    }
}

/// The sandbox a thread's permissions and the persistent "allow always" grants
/// establish for every command in the thread, as a [`ThreadSandbox`]. Full
/// access removes the sandbox entirely, read-only ignores the persistent grants,
/// and otherwise those grants form its scope. The per-thread overrides come from
/// [`ThreadSandboxGrants::thread_sandbox`].
pub fn permissions_thread_sandbox(
    permissions: AgentPermissions,
    persistent: &SandboxPermissions,
) -> ThreadSandbox {
    match SandboxMode::from(permissions) {
        SandboxMode::FullAccess => ThreadSandbox::Unsandboxed,
        SandboxMode::ReadOnly => {
            ThreadSandbox::Sandboxed(settings_sandbox_policy(&SandboxPermissions::default()))
        }
        SandboxMode::WorkspaceWrite => {
            ThreadSandbox::Sandboxed(settings_sandbox_policy(persistent))
        }
    }
}

/// Translate the persistent "allow always" sandbox settings into the
/// cross-platform [`SandboxPolicy`] used for display. This is the "from your
/// settings" half of the sandbox status surface; the per-thread overrides come
/// from [`ThreadSandboxGrants::to_policy`].
pub fn settings_sandbox_policy(persistent: &SandboxPermissions) -> SandboxPolicy {
    let fs = if persistent.allow_fs_write_all {
        SandboxFsPolicy::Unrestricted {
            protected_paths: Vec::new(),
        }
    } else {
        SandboxFsPolicy::Restricted {
            // Route each grant through the verifying reopen (or a fresh capture
            // for legacy bare-string grants) so the enforced path is the one
            // vetted at approval time. A grant that fails to reopen is logged
            // and dropped (fail-closed) rather than silently swallowed, then the
            // survivors are reduced to a minimal cover on their canonical paths.
            writable_paths: sandbox::normalize_host_filesystem_locations(
                persistent
                    .write_paths
                    .iter()
                    .filter_map(acp_thread::granted_write_path_to_location_or_log),
            ),
            protected_paths: Vec::new(),
        }
    };
    SandboxPolicy { fs }
}

/// Whether the sandboxed terminal can be exposed for this project under
/// `mode`.
///
/// Full access turns sandboxing off for the model-facing surface: we expose the
/// plain `terminal` tool and omit the sandbox section from the system prompt,
/// because every command would run without a wrap regardless. A model-requested
/// `unsandboxed: true` escape that the user approves "once" or "for this thread"
/// keeps the sandboxed tool and prompt in place, since the model is still
/// operating in the sandbox model and only escaping individual commands
/// (tracked in `ThreadSandboxGrants`).
pub(crate) fn sandboxing_enabled_for_project(project: &Project, mode: SandboxMode) -> bool {
    sandboxing_available_for_project(project) && mode != SandboxMode::FullAccess
}

/// Whether sandboxing is *applicable* for this project at all — the project is
/// local and the platform has a sandbox integration — independent of the
/// thread's permissions. Used by the UI to distinguish "sandboxing isn't
/// relevant here" (don't show the indicator) from "sandboxing is available but
/// off for this thread" (show it, struck out).
pub(crate) fn sandboxing_available_for_project(project: &Project) -> bool {
    project.is_local()
        && cfg!(any(
            target_os = "macos",
            target_os = "linux",
            target_os = "windows"
        ))
}

/// A request for elevated sandbox permissions for a single terminal command.
///
/// Built from the model-controlled `terminal` tool input after the user has
/// authorized the baseline command. All paths here have already been resolved
/// to absolute, canonicalized paths by the caller — never raw, model-provided
/// strings, and never the model-controlled working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct SandboxRequest {
    /// Allow unrestricted filesystem writes (the broad escape hatch).
    pub allow_fs_write_all: bool,
    /// Run the command fully outside the sandbox.
    pub unsandboxed: bool,
    /// Concrete paths the command needs to write to, each paired with the
    /// canonical target resolved at approval time. Each grants its whole
    /// subtree. These are never globs — write access is always a concrete path subtree
    pub write_paths: Vec<GrantedWritePath>,
}

impl SandboxRequest {
    /// Whether this request asks for anything beyond the default sandbox
    /// scope, and therefore needs user approval.
    pub fn needs_escalation(&self) -> bool {
        self.allow_fs_write_all || self.unsandboxed || !self.write_paths.is_empty()
    }
}

/// In-memory record of the sandbox permissions the user approved "for the
/// rest of the thread".
///
/// Lives on the `Thread` and is shared (via `Rc<RefCell<…>>`) with each tool
/// call's event stream so a later command requesting an already-granted
/// permission can skip the approval prompt. Persistent "allow always" grants
/// are stored separately in [`SandboxPermissions`].
#[derive(Default)]
pub(crate) struct ThreadSandboxGrants {
    allow_fs_write_all: bool,
    unsandboxed: bool,
    /// Whether the user approved running commands *without* a sandbox for the
    /// rest of the thread when the OS sandbox could not be created (the
    /// fallback prompt's "Allow for this thread"). Distinct from
    /// `unsandboxed`, which records a model-requested escape; this is a
    /// user-acknowledged degradation because the sandbox is unavailable.
    sandbox_fallback: bool,
    /// Paths granted write access for the thread, each paired with the canonical
    /// target resolved at approval time. Each covers its whole subtree; redundant
    /// children are pruned on insert (by canonical target).
    write_paths: Vec<GrantedWritePath>,
}

impl ThreadSandboxGrants {
    /// Whether the union of thread grants and persistent "allow always" grants
    /// covers everything `request` asks for, so the command can run without
    /// prompting again.
    ///
    /// Write coverage is pure subtree containment. It is fully deterministic and
    /// never widens scope, because grants are concrete paths rather than globs.
    pub fn covers_with_persistent(
        &self,
        request: &SandboxRequest,
        persistent: &SandboxPermissions,
    ) -> bool {
        if request.unsandboxed {
            // There is no persistent grant for running unsandboxed: full access
            // permissions remove the sandbox from the model-facing surface
            // instead (see `sandboxing_enabled_for_project`). Only a "for this
            // thread" grant suppresses the re-prompt.
            return self.unsandboxed;
        }

        if request.allow_fs_write_all && !(self.allow_fs_write_all || persistent.allow_fs_write_all)
        {
            return false;
        }
        // A full-access write grant covers any concrete write request at the
        // authorization layer; protected paths are enforced by the sandbox.
        if self.allow_fs_write_all || persistent.allow_fs_write_all {
            return true;
        }
        request.write_paths.iter().all(|request| {
            util::paths::path_within_subtree(
                request.canonical_or_requested(),
                self.write_paths
                    .iter()
                    .chain(persistent.write_paths.iter())
                    .map(|granted| granted.canonical_or_requested()),
            )
        })
    }

    /// Whether the user allowed running commands unsandboxed for the rest of
    /// the thread (the fallback prompt's "Allow for this thread"). Distinct
    /// from full access permissions.
    pub fn fallback_granted_for_thread(&self) -> bool {
        self.sandbox_fallback
    }

    /// Whether the user approved running model-requested `unsandboxed: true`
    /// commands for the rest of the thread. Once granted, every command in the
    /// thread runs without a sandbox (the model can no longer scope access),
    /// mirroring the `sandbox_fallback` grant.
    pub fn unsandboxed_granted(&self) -> bool {
        self.unsandboxed
    }

    /// Record that the user approved running commands unsandboxed for the rest
    /// of the thread when the sandbox can't be created. Only the Bubblewrap
    /// sandboxes (Linux directly, Windows via WSL) can fail to create a
    /// sandbox, so this is gated to those platforms.
    #[cfg(any(target_os = "linux", target_os = "windows"))]
    pub fn record_fallback(&mut self) {
        self.sandbox_fallback = true;
    }

    /// The sandbox this thread's grants establish on top of the settings, as a
    /// [`ThreadSandbox`]. A standing "run unsandboxed" grant (a model-requested
    /// escape approved for the thread, or the sandbox-creation fallback) removes
    /// the sandbox entirely; otherwise the granted writable paths form its
    /// scope. This is the "overridden in this thread" half of the sandbox
    /// status surface; the persistent half comes from [`permissions_thread_sandbox`].
    pub fn thread_sandbox(&self) -> ThreadSandbox {
        if self.unsandboxed || self.sandbox_fallback {
            ThreadSandbox::Unsandboxed
        } else {
            ThreadSandbox::Sandboxed(self.to_policy())
        }
    }

    /// Translate the per-thread overrides into the cross-platform
    /// [`SandboxPolicy`] used for display. This is the "overridden in this
    /// thread" half of the sandbox status surface; the persistent half comes
    /// from [`settings_sandbox_policy`].
    pub fn to_policy(&self) -> SandboxPolicy {
        let fs = if self.allow_fs_write_all {
            SandboxFsPolicy::Unrestricted {
                protected_paths: Vec::new(),
            }
        } else {
            SandboxFsPolicy::Restricted {
                // Route each grant through the verifying reopen (or a fresh
                // capture for legacy bare-string grants) so the enforced path is
                // the one vetted at approval time. A grant that fails to reopen
                // is logged and dropped (fail-closed) rather than silently
                // swallowed, then the survivors are reduced to a minimal cover
                // on their canonical paths.
                writable_paths: sandbox::normalize_host_filesystem_locations(
                    self.write_paths
                        .iter()
                        .filter_map(acp_thread::granted_write_path_to_location_or_log),
                ),
                protected_paths: Vec::new(),
            }
        };
        SandboxPolicy { fs }
    }

    /// Serialize these grants for persistence in the thread's database row.
    pub fn to_db(&self) -> crate::db::DbSandboxGrants {
        crate::db::DbSandboxGrants {
            write_paths: self.write_paths.clone(),
            allow_fs_write_all: self.allow_fs_write_all,
            unsandboxed: self.unsandboxed,
            sandbox_fallback: self.sandbox_fallback,
        }
    }

    /// Rebuild thread grants from the persisted form.
    pub fn from_db(db: &crate::db::DbSandboxGrants) -> Self {
        Self {
            allow_fs_write_all: db.allow_fs_write_all,
            unsandboxed: db.unsandboxed,
            sandbox_fallback: db.sandbox_fallback,
            write_paths: db.write_paths.clone(),
        }
    }

    /// Record everything in `request` as granted for the rest of the thread,
    /// pruning entries that become redundant.
    pub fn record(&mut self, request: &SandboxRequest) {
        self.allow_fs_write_all |= request.allow_fs_write_all;
        self.unsandboxed |= request.unsandboxed;
        for granted in &request.write_paths {
            insert_granted_subtree(&mut self.write_paths, granted.clone());
        }
    }

    /// Compute the effective sandbox permissions to enforce for a command: the
    /// union of persistent "allow always" grants, thread grants, and this
    /// specific command's request.
    ///
    /// This is what makes standing grants "stick": every sandboxed command
    /// applies the accumulated grants, so the model can write to a previously
    /// approved path without re-requesting it. Passing the current `request` in also covers "allow
    /// once" grants, which are enforced for this command without being recorded
    /// for the thread.
    pub fn effective_with_persistent(
        &self,
        request: &SandboxRequest,
        persistent: &SandboxPermissions,
    ) -> SandboxRequest {
        let mut write_paths = persistent.write_paths.clone();
        for granted in self.write_paths.iter().chain(request.write_paths.iter()) {
            insert_granted_subtree(&mut write_paths, granted.clone());
        }
        SandboxRequest {
            allow_fs_write_all: persistent.allow_fs_write_all
                || self.allow_fs_write_all
                || request.allow_fs_write_all,
            unsandboxed: request.unsandboxed,
            write_paths,
        }
    }
}

/// Insert `granted` into a write-grant set, keeping it minimal by comparing each
/// entry's canonical (resolved) target. Mirrors [`util::paths::insert_subtree`]:
/// skip the new grant if an existing entry's canonical is an ancestor-or-equal
/// of it, and prune existing entries whose canonical descends from the new one.
fn insert_granted_subtree(subtrees: &mut Vec<GrantedWritePath>, granted: GrantedWritePath) {
    if subtrees.iter().any(|existing| {
        granted
            .canonical_or_requested()
            .starts_with(existing.canonical_or_requested())
    }) {
        return;
    }
    subtrees.retain(|existing| {
        !existing
            .canonical_or_requested()
            .starts_with(granted.canonical_or_requested())
    });
    subtrees.push(granted);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn request(all: bool, paths: &[&str]) -> SandboxRequest {
        SandboxRequest {
            allow_fs_write_all: all,
            unsandboxed: false,
            write_paths: granted(paths),
        }
    }

    /// Bare-string grants (no resolved canonical) for the lexical
    /// coverage/record/dedup tests, which never build a real policy.
    fn granted(paths: &[&str]) -> Vec<GrantedWritePath> {
        paths
            .iter()
            .map(|p| GrantedWritePath::from_requested(PathBuf::from(p)))
            .collect()
    }

    /// A grant carrying the resolved canonical of a real directory, for the
    /// tests that build an actual policy (which reopens/captures real fds).
    fn resolved_grant(dir: &Path) -> GrantedWritePath {
        GrantedWritePath::resolved(
            dir.to_path_buf(),
            sandbox::resolve_canonical(dir).expect("resolve canonical temp dir"),
        )
    }

    fn unsandboxed_request() -> SandboxRequest {
        SandboxRequest {
            allow_fs_write_all: false,
            unsandboxed: true,
            write_paths: Vec::new(),
        }
    }

    #[test]
    fn thread_sandbox_merge_unsandboxed_wins_else_unions_scopes() {
        // Writable paths are captured as real `HostFilesystemLocation`s (which
        // open an fd and key on the inode), so the test uses real directories.
        let dir_a = tempfile::tempdir().expect("create temp dir a");
        let dir_b = tempfile::tempdir().expect("create temp dir b");
        let path_a = dir_a.path();
        let path_b = dir_b.path();

        let policy = |paths: &[&Path]| SandboxPolicy {
            fs: SandboxFsPolicy::Restricted {
                writable_paths: paths
                    .iter()
                    .map(|p| HostFilesystemLocation::capture(p).expect("capture temp dir"))
                    .collect(),
                protected_paths: Vec::new(),
            },
        };

        // Unsandboxed on either side wins — the agent runs with ambient access.
        assert!(
            ThreadSandbox::Unsandboxed
                .merge(ThreadSandbox::Sandboxed(policy(&[path_a])))
                .is_unsandboxed()
        );
        assert!(
            ThreadSandbox::Sandboxed(policy(&[path_a]))
                .merge(ThreadSandbox::Unsandboxed)
                .is_unsandboxed()
        );

        // Two sandboxed layers union their scopes.
        assert_eq!(
            ThreadSandbox::Sandboxed(policy(&[path_a]))
                .merge(ThreadSandbox::Sandboxed(policy(&[path_b]))),
            ThreadSandbox::Sandboxed(policy(&[path_a, path_b]))
        );
    }

    #[test]
    fn permissions_thread_sandbox_follows_the_sandbox_mode() {
        let log_dir = tempfile::tempdir().expect("create temp log dir");
        let persistent = SandboxPermissions {
            write_paths: vec![resolved_grant(log_dir.path())],
            ..Default::default()
        };

        assert!(
            permissions_thread_sandbox(AgentPermissions::FullAccess, &persistent).is_unsandboxed()
        );
        assert_eq!(
            permissions_thread_sandbox(AgentPermissions::Workspace, &persistent),
            ThreadSandbox::Sandboxed(settings_sandbox_policy(&persistent))
        );
        // Read-only ignores the persistent grants.
        assert_eq!(
            permissions_thread_sandbox(AgentPermissions::ReadOnly, &persistent),
            ThreadSandbox::Sandboxed(SandboxPolicy {
                fs: SandboxFsPolicy::Restricted {
                    writable_paths: Vec::new(),
                    protected_paths: Vec::new(),
                },
            })
        );
    }

    #[test]
    fn thread_grants_sandbox_reflects_unsandboxed_grant() {
        let mut grants = ThreadSandboxGrants::default();
        assert!(matches!(
            grants.thread_sandbox(),
            ThreadSandbox::Sandboxed(_)
        ));
        grants.record(&unsandboxed_request());
        assert!(grants.thread_sandbox().is_unsandboxed());
    }

    #[test]
    fn grants_roundtrip_through_db_form() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));
        grants.record(&unsandboxed_request());

        let restored = ThreadSandboxGrants::from_db(&grants.to_db());

        // The restored grants cover exactly what the originals did.
        assert!(covers(&restored, &request(false, &["/tmp/build/cache"])));
        assert!(covers(&restored, &unsandboxed_request()));
        assert_eq!(restored.write_paths, grants.write_paths);
        assert_eq!(restored.unsandboxed, grants.unsandboxed);
    }

    #[test]
    fn db_form_preserves_write_all() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(true, &[]));

        let restored = ThreadSandboxGrants::from_db(&grants.to_db());
        assert!(restored.allow_fs_write_all);
        assert!(covers(&restored, &request(true, &["/anywhere"])));
    }

    #[test]
    fn thread_grants_to_policy_maps_paths() {
        // `to_policy` reopens/captures real `HostFilesystemLocation`s, so use a
        // real dir and a grant carrying its resolved canonical.
        let build_dir = tempfile::tempdir().expect("create temp build dir");

        let mut grants = ThreadSandboxGrants::default();
        grants.record(&SandboxRequest {
            allow_fs_write_all: false,
            unsandboxed: false,
            write_paths: vec![resolved_grant(build_dir.path())],
        });
        let policy = grants.to_policy();
        assert_eq!(
            policy.fs,
            SandboxFsPolicy::Restricted {
                writable_paths: vec![
                    HostFilesystemLocation::capture(build_dir.path()).expect("capture temp dir")
                ],
                protected_paths: Vec::new(),
            }
        );

        // No grants at all: writes restricted to nothing.
        let empty = ThreadSandboxGrants::default().to_policy();
        assert_eq!(
            empty.fs,
            SandboxFsPolicy::Restricted {
                writable_paths: Vec::new(),
                protected_paths: Vec::new(),
            }
        );

        // The broad escape maps to the unrestricted variant.
        let mut broad = ThreadSandboxGrants::default();
        broad.record(&request(true, &[]));
        let policy = broad.to_policy();
        assert_eq!(
            policy.fs,
            SandboxFsPolicy::Unrestricted {
                protected_paths: Vec::new(),
            }
        );
    }

    #[test]
    fn settings_policy_maps_persistent_permissions() {
        // `settings_sandbox_policy` captures real `HostFilesystemLocation`s.
        let log_dir = tempfile::tempdir().expect("create temp log dir");
        let persistent = SandboxPermissions {
            write_paths: vec![resolved_grant(log_dir.path())],
            ..Default::default()
        };
        let policy = settings_sandbox_policy(&persistent);
        assert_eq!(
            policy.fs,
            SandboxFsPolicy::Restricted {
                writable_paths: vec![
                    HostFilesystemLocation::capture(log_dir.path()).expect("capture temp dir")
                ],
                protected_paths: Vec::new(),
            }
        );

        let unrestricted = SandboxPermissions {
            allow_fs_write_all: true,
            ..Default::default()
        };
        let policy = settings_sandbox_policy(&unrestricted);
        assert_eq!(
            policy.fs,
            SandboxFsPolicy::Unrestricted {
                protected_paths: Vec::new(),
            }
        );
    }

    fn covers(grants: &ThreadSandboxGrants, request: &SandboxRequest) -> bool {
        grants.covers_with_persistent(request, &SandboxPermissions::default())
    }

    fn effective(grants: &ThreadSandboxGrants, request: &SandboxRequest) -> SandboxRequest {
        grants.effective_with_persistent(request, &SandboxPermissions::default())
    }

    #[cfg(any(target_os = "linux", target_os = "windows"))]
    #[test]
    fn fallback_granted_for_thread_tracks_record_fallback() {
        let mut grants = ThreadSandboxGrants::default();
        assert!(!grants.fallback_granted_for_thread());

        // The thread-scoped fallback grant is independent of the
        // model-requested `unsandboxed` grant.
        grants.record_fallback();
        assert!(grants.fallback_granted_for_thread());
        assert!(!covers(&grants, &unsandboxed_request()));
    }

    #[test]
    fn empty_grants_cover_nothing() {
        let grants = ThreadSandboxGrants::default();
        assert!(!covers(&grants, &request(true, &[])));
        assert!(!covers(&grants, &unsandboxed_request()));
        assert!(!covers(&grants, &request(false, &["/tmp/build"])));
    }

    #[test]
    fn subtree_containment_covers_children() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));

        // Exact match and any descendant are covered.
        assert!(covers(&grants, &request(false, &["/tmp/build"])));
        assert!(covers(&grants, &request(false, &["/tmp/build/cache"])));
        // A sibling / parent is not.
        assert!(!covers(&grants, &request(false, &["/tmp/other"])));
        assert!(!covers(&grants, &request(false, &["/tmp"])));
    }

    #[test]
    fn record_prunes_redundant_children() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build/cache"]));
        grants.record(&request(false, &["/tmp/build"]));
        assert_eq!(grants.write_paths, granted(&["/tmp/build"]));
    }

    #[test]
    fn record_keeps_existing_broader_grant() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));
        grants.record(&request(false, &["/tmp/build/cache"]));
        assert_eq!(grants.write_paths, granted(&["/tmp/build"]));
    }

    #[test]
    fn all_access_covers_any_concrete_write() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(true, &[]));
        assert!(covers(&grants, &request(false, &["/anywhere/at/all"])));
        // But not running unsandboxed, which wasn't granted.
        assert!(!covers(&grants, &unsandboxed_request()));
    }

    #[test]
    fn unsandboxed_grant_tracked_independently() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&unsandboxed_request());
        assert!(covers(&grants, &unsandboxed_request()));
        assert!(!covers(&grants, &request(true, &[])));
    }

    #[test]
    fn persistent_grants_combine_with_thread_grants() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));
        let persistent = SandboxPermissions {
            write_paths: granted(&["/tmp/always"]),
            ..Default::default()
        };

        assert!(grants.covers_with_persistent(
            &request(false, &["/tmp/build/cache", "/tmp/always/log"]),
            &persistent
        ));
        assert!(!grants.covers_with_persistent(&request(false, &["/tmp/other"]), &persistent));
    }

    #[test]
    fn persistent_all_access_covers_concrete_writes() {
        let grants = ThreadSandboxGrants::default();
        let persistent = SandboxPermissions {
            allow_fs_write_all: true,
            ..Default::default()
        };

        assert!(grants.covers_with_persistent(&request(false, &["/anywhere"]), &persistent));
        assert!(grants.covers_with_persistent(&request(true, &[]), &persistent));
        assert!(!grants.covers_with_persistent(&unsandboxed_request(), &persistent));
    }

    #[test]
    fn thread_grant_covers_unsandboxed_requests() {
        // A "for this thread" grant suppresses the re-prompt for later
        // `unsandboxed: true` requests within the same thread.
        let mut grants = ThreadSandboxGrants::default();
        assert!(!covers(&grants, &unsandboxed_request()));
        grants.record(&unsandboxed_request());
        assert!(covers(&grants, &unsandboxed_request()));

        // A thread-wide unsandboxed grant only covers unsandboxed requests; it
        // does not widen filesystem scope.
        assert!(!covers(&grants, &request(true, &[])));
    }

    #[test]
    fn effective_applies_thread_grants_to_empty_request() {
        // The core fix: a command that requests nothing still gets the
        // thread's granted write paths in its enforced policy.
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));

        let effective = effective(&grants, &request(false, &[]));
        assert_eq!(effective.write_paths, granted(&["/tmp/build"]));
    }

    #[test]
    fn effective_unions_grants_with_once_request() {
        // An "allow once" path (passed via `request`, never recorded) is
        // enforced for this command alongside the standing grants.
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));

        let effective = effective(&grants, &request(false, &["/tmp/once"]));
        assert_eq!(effective.write_paths, granted(&["/tmp/build", "/tmp/once"]));
    }

    #[test]
    fn effective_applies_persistent_grants_to_empty_request() {
        let grants = ThreadSandboxGrants::default();
        let persistent = SandboxPermissions {
            write_paths: granted(&["/tmp/always"]),
            ..Default::default()
        };

        let effective = grants.effective_with_persistent(&request(false, &[]), &persistent);
        assert_eq!(effective.write_paths, granted(&["/tmp/always"]));
    }

    #[test]
    fn effective_dedupes_request_already_covered_by_grant() {
        let mut grants = ThreadSandboxGrants::default();
        grants.record(&request(false, &["/tmp/build"]));

        let effective = effective(&grants, &request(false, &["/tmp/build/cache"]));
        assert_eq!(effective.write_paths, granted(&["/tmp/build"]));
    }
}
