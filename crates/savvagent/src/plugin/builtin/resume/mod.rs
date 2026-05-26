//! `internal:resume` — transcript picker.

pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, HookKind, HostEvent, Manifest, Plugin, PluginError, PluginId,
    PluginKind, Screen, ScreenArgs, ScreenLayout, ScreenSpec, SlashSpec, Timestamp,
    TranscriptHandle,
};

use screen::ResumePickerScreen;

/// Plugin that registers the `/resume` slash command and subscribes to
/// [`HookKind::TranscriptSaved`] to keep an in-memory cache of recent
/// transcripts available without rescanning the filesystem on every open.
pub struct ResumePlugin {
    cache: Vec<TranscriptHandle>,
}

impl ResumePlugin {
    /// Construct a new [`ResumePlugin`], seeding the cache by scanning the
    /// current directory for files matching the default transcript naming
    /// pattern (`transcript-*.json`).
    pub fn new() -> Self {
        let cache = load_recent_transcripts();
        Self { cache }
    }
}

impl Default for ResumePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for ResumePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "resume".into(),
            summary: rust_i18n::t!("slash.resume-summary").to_string(),
            args_hint: None,
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        contributions.screens = vec![ScreenSpec {
            id: "resume.picker".into(),
            layout: ScreenLayout::CenteredModal {
                width_pct: 70,
                height_pct: 70,
                title: Some(rust_i18n::t!("picker.resume.modal-title").to_string()),
            },
        }];
        contributions.hooks = vec![HookKind::TranscriptSaved];

        Manifest {
            id: PluginId::new("internal:resume").expect("valid built-in id"),
            name: "Resume transcript".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.resume-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(&mut self, _: &str, _: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        Ok(vec![Effect::OpenScreen {
            id: "resume.picker".into(),
            args: ScreenArgs::ResumePicker {
                transcripts: self.cache.clone(),
            },
        }])
    }

    fn create_screen(&self, id: &str, args: ScreenArgs) -> Result<Box<dyn Screen>, PluginError> {
        match (id, args) {
            ("resume.picker", ScreenArgs::ResumePicker { transcripts }) => {
                Ok(Box::new(ResumePickerScreen::new(transcripts)))
            }
            (other, _) => Err(PluginError::ScreenNotFound(other.to_string())),
        }
    }

    async fn on_event(&mut self, event: HostEvent) -> Result<Vec<Effect>, PluginError> {
        if let HostEvent::TranscriptSaved { path } = event {
            self.cache.insert(
                0,
                TranscriptHandle {
                    id: path.clone(),
                    label: path,
                    saved_at: Timestamp {
                        secs: now_secs(),
                        nanos: 0,
                    },
                },
            );
            self.cache.truncate(20);
        }
        Ok(vec![])
    }
}

/// Scans the current directory for files matching the default transcript
/// naming pattern (`transcript-*.json`) and returns them sorted newest-first
/// by filename. Returns an empty [`Vec`] if the directory is unreadable.
///
/// v0.9 scans `./*.json` matching the save plugin's default naming pattern.
/// Future versions will scan the user's configured transcript directory.
fn load_recent_transcripts() -> Vec<TranscriptHandle> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(".") {
        Ok(rd) => rd,
        Err(e) => {
            tracing::warn!(error = %e, dir = ".", "load_recent_transcripts: read_dir failed");
            return out;
        }
    };
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(error = %e, "load_recent_transcripts: entry iter error");
                continue;
            }
        };
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if !name.starts_with("transcript-") || !name.ends_with(".json") {
            continue;
        }
        out.push(TranscriptHandle {
            id: name.to_string(),
            label: name.to_string(),
            saved_at: Timestamp {
                secs: now_secs(),
                nanos: 0,
            },
        });
    }
    out.sort_by(|a, b| b.label.cmp(&a.label));
    out
}

/// Returns the current Unix timestamp in whole seconds, or `0` if the system
/// clock is unavailable (e.g. in sandboxed test environments).
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
