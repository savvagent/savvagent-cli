//! `internal:save` — save the active transcript.

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, SlashSpec,
};

/// Plugin that registers the `/save` slash command.
///
/// `/save [path]` emits [`Effect::SaveTranscript`]. The success or failure
/// note is owned by `apply_effects` based on the Result of the actual write.
/// When no path argument is given, [`default_transcript_path`] generates a
/// nanosecond-timestamped filename in the current directory.
pub struct SavePlugin;

impl SavePlugin {
    /// Construct a new [`SavePlugin`].
    pub fn new() -> Self {
        Self
    }
}

impl Default for SavePlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Plugin for SavePlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.slash_commands = vec![SlashSpec {
            name: "save".into(),
            summary: rust_i18n::t!("slash.save-summary").to_string(),
            args_hint: Some("[path]".into()),
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        Manifest {
            id: PluginId::new("internal:save").expect("valid built-in id"),
            name: "Save transcript".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: rust_i18n::t!("plugin.save-description").to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    async fn handle_slash(
        &mut self,
        _: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        let path = args
            .into_iter()
            .next()
            .unwrap_or_else(default_transcript_path);
        Ok(vec![Effect::SaveTranscript { path }])
    }
}

/// Returns a nanosecond-timestamped default transcript filename in the current
/// directory. Using both seconds and nanoseconds prevents same-second
/// collisions when `/save` is called multiple times in quick succession.
///
/// Falls back to `./transcript-0-000000000.json` if the system clock is
/// unavailable.
fn default_transcript_path() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_else(|_| std::time::Duration::from_secs(0));
    format!(
        "./transcript-{}-{:09}.json",
        now.as_secs(),
        now.subsec_nanos()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_with_explicit_path_uses_it() {
        let mut p = SavePlugin::new();
        let effs = p
            .handle_slash("save", vec!["/tmp/x.json".into()])
            .await
            .unwrap();
        assert_eq!(effs.len(), 1);
        assert!(
            matches!(&effs[0], Effect::SaveTranscript { path } if path == "/tmp/x.json"),
            "expected SaveTranscript {{ path: /tmp/x.json }}, got {:?}",
            effs[0]
        );
    }
}
