//! `internal:self-update` — checks for newer releases and self-updates
//! the installed binary.
//!
//! v0.11.0 PR 1 landed the plugin shell + [`InstallMethod`] detection.
//! v0.11.0 PR 2 (this PR) wires the [`HostStarting`](savvagent_plugin::HookKind::HostStarting)
//! hook: on startup the plugin spawns a tokio task that queries the
//! GitHub Releases API, compares against the running binary's version,
//! and stores the result in shared plugin state. The state is read by
//! later PRs from `render_slot` (PR 3) and `handle_slash` (PR 4).
//!
//! See `docs/superpowers/specs/2026-05-13-v0.11.0-tui-self-update-design.md`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Region, SlashSpec, SlotSpec, StyledLine, StyledSpan, TextMods, ThemeColor,
};
use tokio::time::MissedTickBehavior;

/// Install-method detection (pure helper + [`std::env::current_exe`]
/// wrapper). Public to the crate so future PRs in this series can wire
/// it into the `/update` apply path.
pub mod install_method;

/// Version-check logic: GitHub Releases query, semver compare, and the
/// [`UpdateState`] enum that downstream UI surfaces (banner, `/update`)
/// read.
pub mod check;

/// Install path: the [`Installer`] trait and its cargo-dist-backed
/// production impl. Replaces every binary in the release archive (the
/// main `savvagent` binary plus six helpers) by invoking the same
/// installer script that ships with each GitHub Release.
pub mod apply;

/// 24-hour cache for the GitHub Releases query result.
pub mod cache;

pub use apply::{CargoDistInstaller, Installer, apply_update};
pub use check::{GithubReleasesFetcher, ReleasesFetcher, UpdateState, check_for_update};
pub use install_method::{InstallMethod, detect};

use std::sync::OnceLock;

/// Process-wide storage for the "restart to apply" hint. Written by the
/// `/update` success path; read by `main.rs` after the event loop exits
/// so it can print a one-line stderr hint after the alt-screen tears
/// down.
static RESTART_HINT: OnceLock<Mutex<Option<(semver::Version, semver::Version)>>> = OnceLock::new();

fn restart_hint_cell() -> &'static Mutex<Option<(semver::Version, semver::Version)>> {
    RESTART_HINT.get_or_init(|| Mutex::new(None))
}

/// Record a successful binary swap so the host can emit a restart hint
/// on exit. Called once per process when `/update` transitions the
/// plugin to [`UpdateState::Updated`].
fn record_restart_hint(from: semver::Version, to: semver::Version) {
    *restart_hint_cell().lock().unwrap() = Some((from, to));
}

/// Read the restart hint set by [`record_restart_hint`]. Used by
/// `main.rs` after the TUI event loop exits — if `Some`, print a
/// stderr line so the user knows to relaunch.
pub fn pending_restart_hint() -> Option<(semver::Version, semver::Version)> {
    restart_hint_cell().lock().unwrap().clone()
}

/// Environment variable that disables the update check + `/update` apply
/// path entirely. Any non-empty value short-circuits the plugin to
/// [`UpdateState::Disabled`].
const OPT_OUT_ENV_VAR: &str = "SAVVAGENT_NO_UPDATE_CHECK";

/// CLI flag (parsed via `std::env::args`) with the same effect as the
/// env var. Cheaper than pulling in clap for one boolean.
const OPT_OUT_CLI_FLAG: &str = "--no-update-check";

/// Slot id the plugin contributes to. The TUI's `ui.rs` reserves a
/// one-row chunk for this slot above the existing `home.tips` row;
/// `render_slot` returns an empty Vec when there is no update available
/// so the row paints as theme background only.
const BANNER_SLOT_ID: &str = "home.banner";

/// Re-check interval for the periodic loop spawned by `on_event(HostStarting)`.
/// First tick fires immediately (preserves startup behavior); subsequent
/// ticks fire every two hours. Tests override this via
/// [`SelfUpdatePlugin::with_periodic_interval`].
const PERIODIC_INTERVAL: Duration = Duration::from_secs(2 * 60 * 60);

/// Inspect the process environment + argv for the opt-out signal. Pure
/// helper (works on any iterator + env lookup) so unit tests can verify
/// both branches without mutating real env state.
fn opt_out_from(
    env_lookup: impl FnOnce(&str) -> Option<String>,
    args: impl IntoIterator<Item = String>,
) -> bool {
    if env_lookup(OPT_OUT_ENV_VAR)
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    args.into_iter().any(|a| a == OPT_OUT_CLI_FLAG)
}

/// Production wrapper that reads from `std::env` + `std::env::args`.
fn opt_out_active() -> bool {
    opt_out_from(|k| std::env::var(k).ok(), std::env::args())
}

/// Build a `PushNote` effect carrying a plain (un-styled) text line.
/// Centralised so each call site doesn't repeat the `StyledSpan`
/// scaffolding.
fn note_effect(text: String) -> Effect {
    Effect::PushNote {
        line: StyledLine {
            spans: vec![StyledSpan {
                text,
                fg: None,
                bg: None,
                modifiers: TextMods::default(),
            }],
        },
    }
}

/// TUI self-update plugin.
///
/// Holds the install-method classification (captured once at construction)
/// and the in-memory [`UpdateState`] mutated by the `HostStarting` task.
/// Registered as [`PluginKind::Core`] — the v0.11.0 PR 3 opt-out flags
/// (env var + CLI) provide the disable affordance instead of the
/// user-toggle plugin manager.
pub struct SelfUpdatePlugin {
    /// Cached install-method classification. Captured at construction
    /// because `current_exe()` is stable for the process lifetime.
    install_method: InstallMethod,
    /// Shared state mutated by the spawned `HostStarting` task and the
    /// `/update` slash handler; read by `render_slot` via `try_lock`.
    state: Arc<Mutex<UpdateState>>,
    /// Fetcher used by the spawned check task. The default constructor
    /// installs [`GithubReleasesFetcher`]; tests inject a stub via
    /// [`SelfUpdatePlugin::with_fetcher_and_installer`].
    fetcher: Arc<dyn ReleasesFetcher>,
    /// Installer used by the auto-install path and `/update` retries.
    /// Defaults to [`CargoDistInstaller`]; tests substitute a stub.
    installer: Arc<dyn Installer>,
    /// Optional override for the on-disk cache file path. When `None`,
    /// the spawned check task resolves the path via [`cache::cache_path`]
    /// (i.e. `$HOME/.savvagent/update-check.json`). Tests that exercise
    /// `on_event` MUST pass `Some(tempdir.join("update-check.json"))` —
    /// otherwise the production cache-write path inside `on_event` writes
    /// the stub fetcher's tag to the developer's real `$HOME` cache,
    /// poisoning subsequent launches of the installed binary.
    cache_path_override: Option<PathBuf>,
    /// Re-check cadence. Defaults to [`PERIODIC_INTERVAL`]; tests
    /// override via [`SelfUpdatePlugin::with_periodic_interval`] so
    /// `tokio::time::pause()` + `advance()` can drive multiple ticks
    /// without burning a real 2-hour wall clock.
    periodic_interval: Duration,
}

impl SelfUpdatePlugin {
    /// Construct a new [`SelfUpdatePlugin`] backed by the production
    /// [`GithubReleasesFetcher`] and [`CargoDistInstaller`].
    pub fn new() -> Self {
        Self::with_fetcher_and_installer(
            Arc::new(GithubReleasesFetcher),
            Arc::new(CargoDistInstaller),
        )
    }

    /// Construct a [`SelfUpdatePlugin`] with custom fetcher AND installer.
    /// Honors the `SAVVAGENT_NO_UPDATE_CHECK` env var and
    /// `--no-update-check` CLI flag — when either is set the plugin
    /// starts in [`UpdateState::Disabled`] and `on_event` is a no-op.
    pub fn with_fetcher_and_installer(
        fetcher: Arc<dyn ReleasesFetcher>,
        installer: Arc<dyn Installer>,
    ) -> Self {
        let initial = if opt_out_active() {
            UpdateState::Disabled
        } else {
            UpdateState::Unknown
        };
        Self {
            install_method: detect(),
            state: Arc::new(Mutex::new(initial)),
            fetcher,
            installer,
            cache_path_override: None,
            periodic_interval: PERIODIC_INTERVAL,
        }
    }

    /// Test-only: replace the on-disk cache path resolved by the spawned
    /// `HostStarting` task. Required by any test that drives `on_event`
    /// with a stub fetcher — otherwise the production cache-write path
    /// scribbles the stub's tag into the developer's real `$HOME`.
    #[cfg(test)]
    pub fn with_cache_path_override(mut self, path: PathBuf) -> Self {
        self.cache_path_override = Some(path);
        self
    }

    /// Test-only: override the install-method classification captured at
    /// construction. `cargo test` always runs from `target/debug/deps`, so
    /// `detect()` returns [`InstallMethod::Dev`] and short-circuits the
    /// version check; tests that need to exercise the `Installed` cache /
    /// fetch / install path must force the override.
    #[cfg(test)]
    pub fn with_install_method(mut self, method: InstallMethod) -> Self {
        self.install_method = method;
        self
    }

    /// Test-only: override the periodic re-check cadence. Default is
    /// [`PERIODIC_INTERVAL`] (2 hours); tests pass something tiny like
    /// `Duration::from_millis(50)` so they can drive multiple ticks
    /// under `tokio::time::pause()` + `advance()`.
    #[cfg(test)]
    pub fn with_periodic_interval(mut self, interval: Duration) -> Self {
        self.periodic_interval = interval;
        self
    }

    /// Returns the cached install-method classification.
    #[allow(dead_code)]
    pub fn install_method(&self) -> InstallMethod {
        self.install_method
    }

    /// Read the current [`UpdateState`]. Used by tests today; PR 3's
    /// `render_slot` impl reads via `state.try_lock()` directly on the
    /// shared `Arc` for the same reason.
    #[allow(dead_code)]
    pub fn state(&self) -> UpdateState {
        self.state.lock().unwrap().clone()
    }
}

impl Default for SelfUpdatePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SelfUpdatePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.hooks = vec![HookKind::HostStarting];
        contributions.slots = vec![SlotSpec {
            slot_id: BANNER_SLOT_ID.into(),
            priority: 100,
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: "update".into(),
            summary: rust_i18n::t!("self-update.slash-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];

        Manifest {
            id: PluginId::new("internal:self-update").expect("valid built-in id"),
            name: "Self update".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.self-update-description").to_string(),
            kind: PluginKind::Core,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        _args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "update" {
            return Ok(vec![]);
        }

        // Auto-install kicks off the moment the HostStarting check
        // detects a new release, so /update is mostly a retry/status
        // command. Resolve the action under the lock, then release it
        // before any await — install can take several seconds.
        let action = {
            let guard = self.state.lock().unwrap();
            match &*guard {
                UpdateState::Available { current, latest }
                | UpdateState::InstallFailed {
                    current, latest, ..
                } => Action::Install(current.clone(), latest.clone()),
                UpdateState::Installing { latest, .. } => Action::Note(
                    rust_i18n::t!(
                        "self-update.note-install-in-progress",
                        latest = latest.to_string()
                    )
                    .to_string(),
                ),
                UpdateState::Disabled => {
                    Action::Note(rust_i18n::t!("self-update.note-disabled").to_string())
                }
                UpdateState::Updated { to, .. } => Action::Note(
                    rust_i18n::t!("self-update.note-update-ok", latest = to.to_string())
                        .to_string(),
                ),
                UpdateState::Unknown | UpdateState::UpToDate | UpdateState::CheckFailed => {
                    Action::Note(rust_i18n::t!("self-update.note-no-update").to_string())
                }
            }
        };

        match action {
            Action::Note(text) => Ok(vec![note_effect(text)]),
            Action::Install(current, latest) => Ok(run_install(
                Arc::clone(&self.state),
                Arc::clone(&self.installer),
                current,
                latest,
            )
            .await),
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if !matches!(event, HostEvent::HostStarting) {
            return Ok(vec![]);
        }

        // If opt-out was set at construction the initial state is already
        // `Disabled` — skip the spawn so we don't even build a tokio task.
        if matches!(*self.state.lock().unwrap(), UpdateState::Disabled) {
            return Ok(vec![]);
        }

        // Spawn the version check on the runtime so the hook dispatcher
        // returns immediately. The task consults the 24h cache before
        // hitting the network, persists the latest tag on a fresh fetch,
        // and — if the check classifies the release as Available — kicks
        // off the cargo-dist installer in the same task so the user
        // doesn't have to type /update.
        let state = Arc::clone(&self.state);
        let fetcher = Arc::clone(&self.fetcher);
        let installer = Arc::clone(&self.installer);
        let install_method = self.install_method;
        let current_version = env!("CARGO_PKG_VERSION").to_string();
        let cache_path_override = self.cache_path_override.clone();
        let periodic_interval = self.periodic_interval;

        tokio::spawn(async move {
            if matches!(install_method, InstallMethod::Dev) {
                if let Ok(mut guard) = state.lock() {
                    *guard = UpdateState::Disabled;
                }
                return;
            }

            let cache_path = cache_path_override.or_else(cache::cache_path);
            let mut interval = tokio::time::interval(periodic_interval);
            interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
            let mut is_first_tick = true;

            loop {
                interval.tick().await;

                // Snapshot pre-tick state. Skip rules evaluate before the
                // network call so we can short-circuit cheap cases.
                let pre_state = match state.lock() {
                    Ok(g) => g.clone(),
                    Err(_) => return,
                };
                match pre_state {
                    UpdateState::Disabled => return,
                    UpdateState::Updated { .. } => return,
                    UpdateState::Installing { .. } => continue,
                    _ => {}
                }

                run_check_once(
                    &state,
                    &fetcher,
                    &installer,
                    install_method,
                    &current_version,
                    cache_path.as_deref(),
                    is_first_tick,
                    pre_state,
                )
                .await;
                is_first_tick = false;
            }
        });

        Ok(vec![])
    }

    fn render_slot(&self, slot_id: &str, _region: Region) -> Vec<StyledLine> {
        if slot_id != BANNER_SLOT_ID {
            return vec![];
        }
        // `try_lock` keeps the render hot path non-blocking: if the
        // `HostStarting` task happens to hold the lock for a write, we
        // simply skip this frame and the banner shows on the next.
        let Ok(guard) = self.state.try_lock() else {
            return vec![];
        };
        let text = match &*guard {
            UpdateState::Available { current, latest } => rust_i18n::t!(
                "self-update.banner-available",
                current = current.to_string(),
                latest = latest.to_string()
            )
            .to_string(),
            UpdateState::Installing { latest, .. } => {
                rust_i18n::t!("self-update.banner-installing", latest = latest.to_string())
                    .to_string()
            }
            UpdateState::InstallFailed { latest, error, .. } => rust_i18n::t!(
                "self-update.banner-install-failed",
                latest = latest.to_string(),
                err = error.clone()
            )
            .to_string(),
            UpdateState::Updated { to, .. } => {
                rust_i18n::t!("self-update.banner-updated", latest = to.to_string()).to_string()
            }
            _ => return vec![],
        };
        vec![StyledLine {
            spans: vec![StyledSpan {
                text,
                fg: Some(ThemeColor::Accent),
                bg: None,
                modifiers: TextMods::default(),
            }],
        }]
    }
}

/// Internal helper enum: lets `handle_slash` resolve the state-dependent
/// action under the lock and execute it after the lock is dropped.
enum Action {
    Note(String),
    Install(semver::Version, semver::Version),
}

/// Run the installer for `latest` and update plugin `state` accordingly.
/// Shared by the auto-install path in `on_event` and the retry path in
/// `handle_slash`. Mutates `state` to [`UpdateState::Installing`] before
/// awaiting the installer, then to [`UpdateState::Updated`] (recording a
/// restart hint) or [`UpdateState::InstallFailed`] on completion.
///
/// Returns the effects to push into the transcript: nothing in the
/// auto-install path (the banner is the user-visible signal), three
/// notes in the slash path (starting → outcome). The single shared
/// implementation keeps the state-machine in one place; the caller
/// decides whether to surface the returned effects.
async fn run_install(
    state: Arc<Mutex<UpdateState>>,
    installer: Arc<dyn Installer>,
    current: semver::Version,
    latest: semver::Version,
) -> Vec<Effect> {
    let starting_note =
        rust_i18n::t!("self-update.note-updating", latest = latest.to_string()).to_string();

    *state.lock().unwrap() = UpdateState::Installing {
        current: current.clone(),
        latest: latest.clone(),
    };

    match apply_update(installer.as_ref(), current.clone(), latest.clone()).await {
        Ok(new_state) => {
            if let UpdateState::Updated { from, to } = &new_state {
                record_restart_hint(from.clone(), to.clone());
            }
            *state.lock().unwrap() = new_state;
            let ok_note = rust_i18n::t!("self-update.note-update-ok", latest = latest.to_string())
                .to_string();
            vec![note_effect(starting_note), note_effect(ok_note)]
        }
        Err(e) => {
            let error = e.to_string();
            *state.lock().unwrap() = UpdateState::InstallFailed {
                current,
                latest,
                error: error.clone(),
            };
            let fail_note = rust_i18n::t!("self-update.note-update-fail", err = error).to_string();
            vec![note_effect(starting_note), note_effect(fail_note)]
        }
    }
}

/// One pass of the version-check + maybe-install pipeline. Shared by
/// the `HostStarting` interval loop (each tick calls this) and the
/// auto-install path. Stateless aside from the shared `Arc`s and the
/// cache file — safe to call repeatedly.
#[allow(clippy::too_many_arguments)]
async fn run_check_once(
    state: &Arc<Mutex<UpdateState>>,
    fetcher: &Arc<dyn ReleasesFetcher>,
    installer: &Arc<dyn Installer>,
    install_method: InstallMethod,
    current_version: &str,
    cache_path: Option<&std::path::Path>,
    is_first_tick: bool,
    pre_state: UpdateState,
) {
    // 24h cache: if a fresh entry exists on the first tick, skip the
    // network. Tests that exercise this code path pass an explicit
    // override (set via `with_cache_path_override`) so the production
    // cache file under the developer's real `$HOME` is never touched by
    // the suite.
    //
    // A cached `latest_tag` strictly older than the running binary is
    // treated as a cache miss: it implies the user upgraded out-of-band
    // (cargo install, downloaded tarball, package manager) since the
    // cache was written, so we have no authoritative info about what's
    // newer than the current version and must re-fetch.
    //
    // Subsequent ticks always bypass the cache so the periodic loop
    // actually contacts GitHub and can detect new releases published
    // after startup.
    let cached_fresh = if is_first_tick {
        cache_path
            .and_then(cache::load)
            .filter(|e| cache::is_fresh(e, cache::now_unix(), cache::DEFAULT_TTL_SECS))
            .filter(|e| {
                !matches!(
                    check::compare_versions(current_version, &e.latest_tag),
                    check::Comparison::Ahead | check::Comparison::Unparseable
                )
            })
    } else {
        None
    };

    let result = if let Some(entry) = cached_fresh {
        tracing::debug!(tag = %entry.latest_tag, "self-update: using cached tag");
        check::classify_tag(current_version, &entry.latest_tag)
    } else {
        let fresh = check_for_update(current_version, install_method, fetcher.as_ref()).await;
        if let Some(path) = cache_path {
            if let Some(tag) = match &fresh {
                UpdateState::Available { latest, .. } => Some(format!("v{latest}")),
                UpdateState::UpToDate => Some(format!("v{current_version}")),
                _ => None,
            } {
                cache::save(
                    path,
                    &cache::CacheEntry {
                        schema_version: 1,
                        checked_at_unix: cache::now_unix(),
                        latest_tag: tag,
                    },
                );
            }
        }
        fresh
    };

    // Decide what to publish + whether to install based on the pre-tick state.
    //
    // Special case: when pre-state is InstallFailed with tag `failed`, the
    // periodic loop re-runs the check (in case GitHub has published a newer
    // release) but only installs when the live tag differs from `failed`.
    // This avoids hammering a known-broken release while still recovering
    // automatically once a new release lands.
    let (publish, pending_install) = match (&pre_state, &result) {
        // Same-tag install failure: keep the failure context, do nothing.
        (
            UpdateState::InstallFailed { latest: failed, .. },
            UpdateState::Available { latest: new, .. },
        ) if failed == new => (None, None),

        // Live check now says we're up-to-date — clear the InstallFailed banner.
        (UpdateState::InstallFailed { .. }, UpdateState::UpToDate) => {
            (Some(UpdateState::UpToDate), None)
        }

        // Network failure with no new info — preserve the InstallFailed state.
        (UpdateState::InstallFailed { .. }, UpdateState::CheckFailed) => (None, None),

        // Either pre-state is not InstallFailed, or it is and the live tag
        // differs. Publish the new classification; install if Available.
        _ => {
            let install = if let UpdateState::Available { current, latest } = &result {
                Some((current.clone(), latest.clone()))
            } else {
                None
            };
            (Some(result), install)
        }
    };

    if let Some(new_state) = publish {
        if let Ok(mut guard) = state.lock() {
            *guard = new_state;
        }
    }

    if let Some((current, latest)) = pending_install {
        let _ = run_install(Arc::clone(state), Arc::clone(installer), current, latest).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::HOME_LOCK;
    use async_trait::async_trait;
    use semver::Version;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In-test releases fetcher that returns a fixed tag.
    struct FixedFetcher(&'static str);

    #[async_trait]
    impl ReleasesFetcher for FixedFetcher {
        async fn latest_tag(&self) -> anyhow::Result<String> {
            Ok(self.0.to_string())
        }
    }

    /// In-test releases fetcher that returns a fixed tag and counts
    /// how many times `latest_tag()` is invoked. Used by tests that
    /// need to assert the periodic loop is actually running ticks.
    struct CountingFetcher {
        tag: &'static str,
        invocations: AtomicUsize,
    }

    impl CountingFetcher {
        fn new(tag: &'static str) -> Self {
            Self {
                tag,
                invocations: AtomicUsize::new(0),
            }
        }
        fn invocation_count(&self) -> usize {
            self.invocations.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl ReleasesFetcher for CountingFetcher {
        async fn latest_tag(&self) -> anyhow::Result<String> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(self.tag.to_string())
        }
    }

    /// In-test installer that parks `install()` on `release` until the
    /// test explicitly releases it, and signals `parked` immediately
    /// after incrementing the invocation count so the test can
    /// synchronize on "the install has parked" without relying on
    /// cooperative-scheduling yield depths.
    struct BlockingStubInstaller {
        release: Arc<tokio::sync::Notify>,
        parked: Arc<tokio::sync::Notify>,
        invocations: AtomicUsize,
    }

    impl BlockingStubInstaller {
        fn new(release: Arc<tokio::sync::Notify>, parked: Arc<tokio::sync::Notify>) -> Self {
            Self {
                release,
                parked,
                invocations: AtomicUsize::new(0),
            }
        }
        fn invocation_count(&self) -> usize {
            self.invocations.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Installer for BlockingStubInstaller {
        async fn install(&self, _latest: &Version) -> anyhow::Result<()> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            // Signal the test that we have entered install and are about
            // to park. Notify::notify_one stores a permit if no waiter is
            // present, so ordering with the test's notified().await is
            // safe regardless of which arrives first.
            self.parked.notify_one();
            self.release.notified().await;
            Ok(())
        }
    }

    /// In-test installer: records each `install()` invocation and returns
    /// the configured outcome. Defaults to success; `with_error` flips
    /// the outcome to a fixed error message.
    struct StubInstaller {
        result: Mutex<Result<(), String>>,
        invocations: AtomicUsize,
        last_version: Mutex<Option<Version>>,
    }

    impl StubInstaller {
        fn ok() -> Self {
            Self {
                result: Mutex::new(Ok(())),
                invocations: AtomicUsize::new(0),
                last_version: Mutex::new(None),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                result: Mutex::new(Err(msg.into())),
                invocations: AtomicUsize::new(0),
                last_version: Mutex::new(None),
            }
        }
        fn invocation_count(&self) -> usize {
            self.invocations.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Installer for StubInstaller {
        async fn install(&self, latest: &Version) -> anyhow::Result<()> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            *self.last_version.lock().unwrap() = Some(latest.clone());
            match &*self.result.lock().unwrap() {
                Ok(()) => Ok(()),
                Err(msg) => Err(anyhow::anyhow!(msg.clone())),
            }
        }
    }

    /// Construct a plugin pre-seeded into the supplied state, with a
    /// stubbed fetcher (never invoked here) and the supplied installer.
    /// The HOME_LOCK guard is held only long enough to set the locale +
    /// build the plugin; clippy's `await_holding_lock` is a hard error
    /// in this crate.
    fn locked_plugin_with_state(
        installer: Arc<StubInstaller>,
        state: UpdateState,
    ) -> SelfUpdatePlugin {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");
        let p = SelfUpdatePlugin::with_fetcher_and_installer(
            Arc::new(FixedFetcher("v0.11.0")),
            installer,
        );
        *p.state.lock().unwrap() = state;
        p
    }

    fn dummy_region() -> Region {
        Region {
            x: 0,
            y: 0,
            width: 80,
            height: 1,
        }
    }

    #[test]
    fn manifest_subscribes_to_host_starting_contributes_slot_and_slash() {
        let _lock = HOME_LOCK.lock().unwrap();
        rust_i18n::set_locale("en");

        let p = SelfUpdatePlugin::new();
        let m = p.manifest();
        assert_eq!(m.id.as_str(), "internal:self-update");
        assert_eq!(m.contributions.hooks, vec![HookKind::HostStarting]);
        assert_eq!(m.contributions.slots.len(), 1);
        assert_eq!(m.contributions.slots[0].slot_id, BANNER_SLOT_ID);
        assert_eq!(m.contributions.slash_commands.len(), 1);
        assert_eq!(m.contributions.slash_commands[0].name, "update");
    }

    #[test]
    fn initial_state_is_unknown_when_not_opted_out() {
        // The plugin reads real env/argv on construction; this test runs
        // under cargo test, whose argv does not include the opt-out flag
        // and whose env (in CI / typical local dev) does not set
        // SAVVAGENT_NO_UPDATE_CHECK. If the developer happens to have
        // that env var set, this assertion is a no-op rather than a
        // misleading failure.
        if std::env::var(OPT_OUT_ENV_VAR)
            .ok()
            .is_some_and(|v| !v.is_empty())
        {
            return;
        }
        let p = SelfUpdatePlugin::new();
        assert_eq!(p.state(), UpdateState::Unknown);
    }

    // --- opt_out_from pure helper ---

    #[test]
    fn opt_out_env_var_with_value_disables() {
        assert!(opt_out_from(
            |k| if k == OPT_OUT_ENV_VAR {
                Some("1".into())
            } else {
                None
            },
            std::iter::empty::<String>(),
        ));
    }

    #[test]
    fn opt_out_env_var_empty_does_not_disable() {
        assert!(!opt_out_from(
            |k| if k == OPT_OUT_ENV_VAR {
                Some(String::new())
            } else {
                None
            },
            std::iter::empty::<String>(),
        ));
    }

    #[test]
    fn opt_out_cli_flag_disables() {
        assert!(opt_out_from(
            |_| None,
            vec!["savvagent".to_string(), "--no-update-check".to_string()],
        ));
    }

    #[test]
    fn opt_out_returns_false_when_neither_set() {
        assert!(!opt_out_from(|_| None, vec!["savvagent".to_string()]));
    }

    // --- render_slot ---

    #[test]
    fn render_slot_returns_empty_for_unknown_state() {
        let p = locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::Unknown);
        assert!(p.render_slot(BANNER_SLOT_ID, dummy_region()).is_empty());
    }

    #[test]
    fn render_slot_returns_empty_for_up_to_date() {
        let p = locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::UpToDate);
        assert!(p.render_slot(BANNER_SLOT_ID, dummy_region()).is_empty());
    }

    #[test]
    fn render_slot_returns_empty_for_disabled() {
        let p = locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::Disabled);
        assert!(p.render_slot(BANNER_SLOT_ID, dummy_region()).is_empty());
    }

    #[test]
    fn render_slot_renders_banner_when_available() {
        let p = locked_plugin_with_state(
            Arc::new(StubInstaller::ok()),
            UpdateState::Available {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans[0].text;
        assert!(
            text.contains("0.10.0") && text.contains("0.11.0"),
            "expected both versions in banner, got: {text}"
        );
    }

    #[test]
    fn render_slot_renders_installing_banner() {
        let p = locked_plugin_with_state(
            Arc::new(StubInstaller::ok()),
            UpdateState::Installing {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans[0].text;
        assert!(
            text.contains("0.11.0"),
            "expected latest version in installing banner: {text}"
        );
    }

    #[test]
    fn render_slot_renders_install_failed_banner() {
        let p = locked_plugin_with_state(
            Arc::new(StubInstaller::ok()),
            UpdateState::InstallFailed {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
                error: "network down".into(),
            },
        );
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans[0].text;
        assert!(
            text.contains("0.11.0") && text.contains("network down"),
            "expected version + error in install-failed banner: {text}"
        );
        assert!(
            text.contains("/update"),
            "expected retry hint pointing at /update: {text}"
        );
    }

    #[test]
    fn render_slot_ignores_other_slot_ids() {
        let p = locked_plugin_with_state(
            Arc::new(StubInstaller::ok()),
            UpdateState::Available {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );
        assert!(p.render_slot("home.tips", dummy_region()).is_empty());
    }

    #[test]
    fn render_slot_renders_updated_banner_when_state_is_updated() {
        let p = locked_plugin_with_state(
            Arc::new(StubInstaller::ok()),
            UpdateState::Updated {
                from: Version::parse("0.10.0").unwrap(),
                to: Version::parse("0.11.0").unwrap(),
            },
        );
        let lines = p.render_slot(BANNER_SLOT_ID, dummy_region());
        assert_eq!(lines.len(), 1);
        let text = &lines[0].spans[0].text;
        assert!(
            text.contains("0.11.0"),
            "expected 'to' version in banner: {text}"
        );
        assert!(
            text.to_lowercase().contains("restart"),
            "expected restart hint in banner: {text}"
        );
    }

    // --- /update handle_slash ---

    #[tokio::test]
    async fn slash_update_when_available_runs_installer_and_transitions_to_updated() {
        let installer = Arc::new(StubInstaller::ok());
        let mut plugin = locked_plugin_with_state(
            installer.clone(),
            UpdateState::Available {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );

        let effects = plugin.handle_slash("update", vec![]).await.unwrap();

        assert_eq!(installer.invocation_count(), 1);
        // Two notes pushed: starting + success.
        assert_eq!(effects.len(), 2);

        match plugin.state() {
            UpdateState::Updated { from, to } => {
                assert_eq!(from.to_string(), "0.10.0");
                assert_eq!(to.to_string(), "0.11.0");
            }
            other => panic!("expected Updated state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_update_when_install_fails_transitions_to_install_failed() {
        let installer = Arc::new(StubInstaller::err("network down"));
        let mut plugin = locked_plugin_with_state(
            installer,
            UpdateState::Available {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );

        let effects = plugin.handle_slash("update", vec![]).await.unwrap();

        // Two notes: starting + failure (with err text passthrough).
        assert_eq!(effects.len(), 2);
        if let Effect::PushNote { line } = &effects[1] {
            assert!(
                line.spans[0].text.contains("network down"),
                "fail note must include error: {}",
                line.spans[0].text
            );
        } else {
            panic!("expected PushNote effect");
        }

        match plugin.state() {
            UpdateState::InstallFailed { latest, error, .. } => {
                assert_eq!(latest.to_string(), "0.11.0");
                assert!(error.contains("network down"), "got error: {error}");
            }
            other => panic!("expected InstallFailed state, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn slash_update_when_install_failed_retries_install() {
        let installer = Arc::new(StubInstaller::ok());
        let mut plugin = locked_plugin_with_state(
            installer.clone(),
            UpdateState::InstallFailed {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
                error: "previous failure".into(),
            },
        );

        let effects = plugin.handle_slash("update", vec![]).await.unwrap();

        assert_eq!(
            installer.invocation_count(),
            1,
            "/update on InstallFailed must re-run the installer"
        );
        assert_eq!(effects.len(), 2);
        assert!(matches!(plugin.state(), UpdateState::Updated { .. }));
    }

    #[tokio::test]
    async fn slash_update_during_installing_returns_in_progress_note_only() {
        let installer = Arc::new(StubInstaller::ok());
        let mut plugin = locked_plugin_with_state(
            installer.clone(),
            UpdateState::Installing {
                current: Version::parse("0.10.0").unwrap(),
                latest: Version::parse("0.11.0").unwrap(),
            },
        );

        let effects = plugin.handle_slash("update", vec![]).await.unwrap();

        assert_eq!(effects.len(), 1);
        assert_eq!(
            installer.invocation_count(),
            0,
            "must not re-enter the installer while one is already running"
        );
        assert!(matches!(plugin.state(), UpdateState::Installing { .. }));
    }

    #[tokio::test]
    async fn slash_update_when_no_update_returns_no_update_note() {
        let mut plugin =
            locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::UpToDate);

        let effects = plugin.handle_slash("update", vec![]).await.unwrap();
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::PushNote { .. }));
    }

    #[tokio::test]
    async fn slash_update_when_disabled_returns_disabled_note() {
        let mut plugin =
            locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::Disabled);
        let effects = plugin.handle_slash("update", vec![]).await.unwrap();
        assert_eq!(effects.len(), 1);
    }

    #[tokio::test]
    async fn slash_ignores_other_commands() {
        let mut plugin =
            locked_plugin_with_state(Arc::new(StubInstaller::ok()), UpdateState::UpToDate);
        let effects = plugin.handle_slash("not-update", vec![]).await.unwrap();
        assert!(effects.is_empty());
    }

    // --- on_event auto-install path ---

    fn make_on_event_plugin(
        fetcher: Arc<dyn ReleasesFetcher>,
        installer: Arc<StubInstaller>,
        cache_path: std::path::PathBuf,
    ) -> SelfUpdatePlugin {
        SelfUpdatePlugin::with_fetcher_and_installer(fetcher, installer)
            .with_cache_path_override(cache_path)
    }

    /// Spin until the plugin's state matches `predicate` or the iteration
    /// budget is exhausted. Uses `sleep(Duration::ZERO)` so each iteration
    /// drives the tokio timer driver (required now that the spawned task
    /// awaits `interval.tick()` before calling `run_check_once`).
    async fn wait_for_state(
        plugin: &SelfUpdatePlugin,
        predicate: impl Fn(&UpdateState) -> bool,
    ) -> UpdateState {
        for _ in 0..200 {
            tokio::time::sleep(Duration::ZERO).await;
            let s = plugin.state();
            if predicate(&s) {
                return s;
            }
        }
        panic!(
            "state predicate never matched; final state: {:?}",
            plugin.state()
        );
    }

    #[tokio::test]
    async fn host_starting_auto_installs_on_available_then_writes_cache() {
        // The cache-path override is mandatory: `on_event` writes the
        // fetched tag to disk, and without the override the production
        // path would scribble `v99.99.99` into the developer's real
        // `~/.savvagent/update-check.json`, poisoning the next 24h of
        // launches for the installed binary.
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let installer = Arc::new(StubInstaller::ok());
        let mut p = make_on_event_plugin(
            Arc::new(FixedFetcher("v99.99.99")),
            installer.clone(),
            cache_path.clone(),
        );
        let install_method = p.install_method();

        p.on_event(HostEvent::HostStarting).await.unwrap();

        let final_state = wait_for_state(&p, |s| {
            !matches!(
                s,
                UpdateState::Unknown
                    | UpdateState::Available { .. }
                    | UpdateState::Installing { .. }
            )
        })
        .await;

        match install_method {
            InstallMethod::Dev => {
                assert_eq!(final_state, UpdateState::Disabled);
                assert!(
                    !cache_path.exists(),
                    "dev short-circuit must not write cache"
                );
                assert_eq!(
                    installer.invocation_count(),
                    0,
                    "dev short-circuit must not invoke the installer"
                );
            }
            InstallMethod::Installed => {
                assert!(
                    matches!(final_state, UpdateState::Updated { .. }),
                    "expected auto-install to land in Updated, got: {final_state:?}"
                );
                assert_eq!(installer.invocation_count(), 1);
                let entry = cache::load(&cache_path)
                    .expect("cache file must be written at the override path");
                assert_eq!(entry.latest_tag, "v99.99.99");
            }
        }
    }

    #[tokio::test]
    async fn host_starting_install_failure_transitions_to_install_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let installer = Arc::new(StubInstaller::err("download failed"));
        let mut p = make_on_event_plugin(
            Arc::new(FixedFetcher("v99.99.99")),
            installer.clone(),
            cache_path,
        );

        // Skip the assertion on Dev hosts; the dev short-circuit bypasses
        // the installer entirely so the failure path can't be exercised.
        if matches!(p.install_method(), InstallMethod::Dev) {
            return;
        }

        p.on_event(HostEvent::HostStarting).await.unwrap();

        let final_state =
            wait_for_state(&p, |s| matches!(s, UpdateState::InstallFailed { .. })).await;

        match final_state {
            UpdateState::InstallFailed { error, latest, .. } => {
                assert_eq!(latest.to_string(), "99.99.99");
                assert!(error.contains("download failed"), "got: {error}");
            }
            other => unreachable!("predicate guarantees InstallFailed: {other:?}"),
        }
        assert_eq!(installer.invocation_count(), 1);
    }

    #[tokio::test]
    async fn other_events_are_ignored() {
        // Even though `on_event` currently returns early for non-HostStarting
        // events, point the cache override at a tempdir so any future
        // change that lifts the early-return cannot silently start writing
        // to the developer's real `$HOME` cache.
        let tmp = tempfile::tempdir().unwrap();
        let installer = Arc::new(StubInstaller::ok());
        let mut p = make_on_event_plugin(
            Arc::new(FixedFetcher("v99.99.99")),
            installer.clone(),
            tmp.path().join("update-check.json"),
        );

        p.on_event(HostEvent::TurnStart { turn_id: 1 })
            .await
            .unwrap();
        tokio::task::yield_now().await;
        assert_eq!(p.state(), UpdateState::Unknown);
        assert_eq!(installer.invocation_count(), 0);
    }

    #[tokio::test]
    async fn host_starting_bypasses_cache_when_current_version_is_ahead_of_cached_tag() {
        // Regression: a cache entry written while the binary was on an older
        // version stays "fresh" for 24h. If the user upgrades out-of-band
        // (cargo install, downloaded tarball, package manager) within that
        // window, the cached `latest_tag` is now older than the running
        // binary. The old code accepted the stale cache, classified the
        // running version as `Ahead` -> `UpToDate`, and silently hid any
        // genuinely newer release that GitHub now publishes. The plugin
        // must instead treat that cache as uninformative and re-fetch.
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");

        // Pre-populate a fresh cache (written "now") whose tag is older
        // than `CARGO_PKG_VERSION`. v0.1.0 is unambiguously below any
        // release the workspace will ship for the lifetime of this fix.
        cache::save(
            &cache_path,
            &cache::CacheEntry {
                schema_version: 1,
                checked_at_unix: cache::now_unix(),
                latest_tag: "v0.1.0".into(),
            },
        );

        let installer = Arc::new(StubInstaller::ok());
        let mut p = make_on_event_plugin(
            Arc::new(FixedFetcher("v99.99.99")),
            installer.clone(),
            cache_path.clone(),
        )
        // `cargo test` runs under `target/debug/deps/...`, so without the
        // override the plugin short-circuits to Disabled before it touches
        // the cache and never exercises the bug.
        .with_install_method(InstallMethod::Installed);

        p.on_event(HostEvent::HostStarting).await.unwrap();

        let final_state = wait_for_state(&p, |s| matches!(s, UpdateState::Updated { .. })).await;

        match final_state {
            UpdateState::Updated { to, .. } => assert_eq!(to.to_string(), "99.99.99"),
            other => unreachable!("predicate guarantees Updated: {other:?}"),
        }
        assert_eq!(
            installer.invocation_count(),
            1,
            "stale cache must not prevent auto-install"
        );

        // Cache must be rewritten with the freshly fetched tag.
        let entry = cache::load(&cache_path).expect("cache must be rewritten on re-fetch");
        assert_eq!(entry.latest_tag, "v99.99.99");
    }

    /// Advance virtual time and yield enough that the spawned task can
    /// observe the tick. Pause must already be active via
    /// `#[tokio::test(start_paused = true)]`.
    async fn advance_and_yield(by: Duration) {
        tokio::time::advance(by).await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    /// Drive `n` periodic ticks by advancing virtual time `n` times at
    /// `step` each. Necessary because `MissedTickBehavior::Delay` (set on
    /// the production interval) collapses a single large advance into
    /// exactly ONE tick — it skips missed ticks and resumes after now.
    /// To produce multiple ticks we must advance step-by-step.
    async fn advance_n_ticks(n: usize, step: Duration) {
        for _ in 0..n {
            advance_and_yield(step).await;
        }
    }

    /// Spin until the plugin's state matches `predicate`, advancing virtual
    /// time by `step` between checks. Bounded to 200 iterations.
    async fn advance_until_state(
        plugin: &SelfUpdatePlugin,
        step: Duration,
        predicate: impl Fn(&UpdateState) -> bool,
    ) -> UpdateState {
        for _ in 0..200 {
            advance_and_yield(step).await;
            let s = plugin.state();
            if predicate(&s) {
                return s;
            }
        }
        panic!(
            "state predicate never matched; final state: {:?}",
            plugin.state()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_check_runs_multiple_ticks() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        // Tag below CARGO_PKG_VERSION → classification lands at UpToDate
        // (the "Ahead" branch in compare_versions), keeping the loop alive.
        let fetcher = Arc::new(CountingFetcher::new("v0.0.1"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        // Force the timer driver to fire the immediate startup tick.
        advance_and_yield(Duration::ZERO).await;
        // Drive three periodic ticks (Delay-mode interval requires
        // advancing one step at a time).
        advance_n_ticks(3, interval).await;

        assert!(
            fetcher.invocation_count() >= 3,
            "expected ≥3 fetch invocations (1 startup + ≥2 periodic), got {}",
            fetcher.invocation_count()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_check_breaks_after_updated() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        let fetcher = Arc::new(CountingFetcher::new("v99.99.99"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        advance_until_state(&p, interval, |s| matches!(s, UpdateState::Updated { .. })).await;

        // Loop must have broken; advance well past the next interval and
        // confirm the fetcher and installer were not re-invoked. Under
        // MissedTickBehavior::Delay, a single large advance fires AT MOST
        // ONE tick (it skips missed ticks and resumes after now), so this
        // tightly bounds any rogue post-Updated tick at exactly 1.
        advance_and_yield(interval * 5).await;
        assert_eq!(
            installer.invocation_count(),
            1,
            "installer must run exactly once before the Updated break"
        );
        assert_eq!(
            fetcher.invocation_count(),
            1,
            "fetcher must run exactly once before the Updated break"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_check_does_not_start_in_dev() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        let fetcher = Arc::new(CountingFetcher::new("v99.99.99"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Dev)
            .with_periodic_interval(interval)
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        advance_and_yield(interval * 5).await;

        assert_eq!(fetcher.invocation_count(), 0);
        assert_eq!(installer.invocation_count(), 0);
        assert_eq!(p.state(), UpdateState::Disabled);
    }

    #[tokio::test(start_paused = true)]
    async fn first_tick_uses_cache_subsequent_tick_bypasses() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        // Pre-write a fresh cache entry with the current version as the tag,
        // so it is a startup cache HIT (not stale-vs-current) and classifies
        // to UpToDate.
        let current = env!("CARGO_PKG_VERSION");
        cache::save(
            &cache_path,
            &cache::CacheEntry {
                schema_version: 1,
                checked_at_unix: cache::now_unix(),
                latest_tag: format!("v{current}"),
            },
        );

        let fetcher = Arc::new(CountingFetcher::new("v99.99.99"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        // Force the timer driver to fire the immediate startup tick.
        advance_and_yield(Duration::ZERO).await;
        assert_eq!(
            fetcher.invocation_count(),
            0,
            "first tick must use the on-disk cache when fresh and not stale"
        );

        // Second tick must bypass the cache and hit the fetcher.
        advance_and_yield(interval).await;
        assert!(
            fetcher.invocation_count() >= 1,
            "subsequent tick must bypass cache and call the fetcher, got {}",
            fetcher.invocation_count()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn periodic_check_skips_during_concurrent_slash_install() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        let release = Arc::new(tokio::sync::Notify::new());
        let parked = Arc::new(tokio::sync::Notify::new());
        let installer = Arc::new(BlockingStubInstaller::new(
            Arc::clone(&release),
            Arc::clone(&parked),
        ));
        let fetcher = Arc::new(FixedFetcher("v99.99.99"));

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };
        // Pre-seed Available so handle_slash will pick the Install branch
        // immediately (no need to wait for the loop's first tick to set it).
        *p.state.lock().unwrap() = UpdateState::Available {
            current: Version::parse("0.10.0").unwrap(),
            latest: Version::parse("99.99.99").unwrap(),
        };

        // STEP A: Start the slash task BEFORE the periodic loop. The slash
        // helper shares the primary plugin's state + installer via Arc, so
        // the install it triggers mutates `p.state` directly. helper.handle_slash
        // calls run_install -> sets state to Installing -> awaits the notify.
        let shared_state = Arc::clone(&p.state);
        let shared_installer = Arc::clone(&installer) as Arc<dyn Installer>;
        let slash_task = tokio::spawn(async move {
            let mut helper = SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::new(FixedFetcher("v99.99.99")),
                shared_installer,
            )
            .with_install_method(InstallMethod::Installed);
            helper.state = shared_state;
            let _ = helper.handle_slash("update", vec![]).await;
        });

        // Wait for the slash task to enter the installer and signal it
        // has parked. Explicit synchronization replaces a yield-count
        // bet on the run_install / handle_slash yield depth.
        parked.notified().await;
        assert!(
            matches!(p.state(), UpdateState::Installing { .. }),
            "slash task must park at installer with state Installing, got {:?}",
            p.state()
        );
        assert_eq!(installer.invocation_count(), 1);

        // STEP B: Now start the periodic loop. Its first tick must observe
        // Installing (set by the slash task) and skip.
        p.on_event(HostEvent::HostStarting).await.unwrap();

        // Drive three periodic ticks. The loop must observe Installing on
        // every iteration and skip — no second installer invocation.
        advance_n_ticks(3, interval).await;
        assert_eq!(
            installer.invocation_count(),
            1,
            "loop must skip while state is Installing; got {} invocations",
            installer.invocation_count()
        );

        // Release the notify; the slash task's install completes and the
        // shared state transitions to Updated.
        release.notify_one();
        let _ = slash_task.await;
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
        assert!(
            matches!(p.state(), UpdateState::Updated { .. }),
            "expected Updated after install completes, got {:?}",
            p.state()
        );
        assert_eq!(installer.invocation_count(), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn install_failed_periodic_skips_install_when_tag_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        let fetcher = Arc::new(CountingFetcher::new("v99.99.99"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };
        // Pre-seed with the SAME tag the fetcher will return.
        *p.state.lock().unwrap() = UpdateState::InstallFailed {
            current: Version::parse("0.10.0").unwrap(),
            latest: Version::parse("99.99.99").unwrap(),
            error: "previous failure".into(),
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        advance_n_ticks(3, interval).await;

        assert_eq!(
            installer.invocation_count(),
            0,
            "must not re-attempt install when live tag matches previously failed tag"
        );
        assert!(
            matches!(p.state(), UpdateState::InstallFailed { .. }),
            "state must remain InstallFailed; got {:?}",
            p.state()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn install_failed_periodic_installs_when_new_tag_appears() {
        let tmp = tempfile::tempdir().unwrap();
        let cache_path = tmp.path().join("update-check.json");
        let interval = Duration::from_millis(50);

        // Fetcher returns 99.99.99; pre-seed InstallFailed with 99.99.98
        // so the live tag is genuinely newer than the failed one.
        let fetcher = Arc::new(CountingFetcher::new("v99.99.99"));
        let installer = Arc::new(StubInstaller::ok());

        let mut p = {
            let _lock = HOME_LOCK.lock().unwrap();
            rust_i18n::set_locale("en");
            SelfUpdatePlugin::with_fetcher_and_installer(
                Arc::clone(&fetcher) as Arc<dyn ReleasesFetcher>,
                Arc::clone(&installer) as Arc<dyn Installer>,
            )
            .with_cache_path_override(cache_path)
            .with_install_method(InstallMethod::Installed)
            .with_periodic_interval(interval)
        };
        *p.state.lock().unwrap() = UpdateState::InstallFailed {
            current: Version::parse("0.10.0").unwrap(),
            latest: Version::parse("99.99.98").unwrap(),
            error: "previous failure".into(),
        };

        p.on_event(HostEvent::HostStarting).await.unwrap();
        let final_state =
            advance_until_state(&p, interval, |s| matches!(s, UpdateState::Updated { .. })).await;

        assert_eq!(installer.invocation_count(), 1);
        match final_state {
            UpdateState::Updated { to, .. } => assert_eq!(to.to_string(), "99.99.99"),
            other => unreachable!("predicate guarantees Updated: {other:?}"),
        }
    }
}
