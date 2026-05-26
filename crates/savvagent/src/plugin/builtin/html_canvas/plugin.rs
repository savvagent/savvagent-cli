use async_trait::async_trait;
use savvagent_canvas::HtmlCanvas;
use savvagent_plugin::{
    ContentBlockId, ContentRenderer, ContentRendererSpec, Contributions, Manifest, Plugin,
    PluginError, PluginId, PluginKind, SlashSpec, SystemPromptSegment,
};

use super::prompt_text::{DEFAULT_PROMPT_ID, DEFAULT_PROMPT_TEXT};

/// `internal:html-canvas` plugin. Constructed by `register_builtins`.
#[derive(Debug, Default)]
pub struct HtmlCanvasPlugin;

impl HtmlCanvasPlugin {
    /// Construct a new instance.
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Plugin for HtmlCanvasPlugin {
    fn manifest(&self) -> Manifest {
        let mut contributions = Contributions::default();
        contributions.content_renderers = vec![ContentRendererSpec {
            block_type: "html".to_string(),
            canonical: true,
        }];
        contributions.prompt_segments = vec![SystemPromptSegment {
            id: DEFAULT_PROMPT_ID.to_string(),
            text: DEFAULT_PROMPT_TEXT.to_string(),
        }];
        contributions.slash_commands = vec![SlashSpec {
            name: "save-canvas".to_string(),
            summary: "Save the most recent HTML canvas to a file".to_string(),
            args_hint: Some("[path] [--block N] [--open]".to_string()),
            requires_arg: false,
            suppress_prompt_segments: vec![],
        }];
        Manifest {
            id: PluginId::new("internal:html-canvas").expect("valid built-in id"),
            name: "HTML canvas".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            description: "Renders model-authored HTML inline as a static canvas.".to_string(),
            kind: PluginKind::Optional,
            contributions,
        }
    }

    fn create_renderer(
        &self,
        block_type: &str,
        id: ContentBlockId,
        source: &str,
    ) -> Result<Box<dyn ContentRenderer>, PluginError> {
        match block_type {
            "html" => Ok(Box::new(HtmlCanvas::new(id, source))),
            other => Err(PluginError::ContentRendererNotFound(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_advertises_html_renderer_and_prompt_segment() {
        let p = HtmlCanvasPlugin;
        let m = p.manifest();
        assert_eq!(m.id, PluginId::new("internal:html-canvas").unwrap());
        assert_eq!(m.kind, PluginKind::Optional);
        assert_eq!(m.contributions.content_renderers.len(), 1);
        assert_eq!(m.contributions.content_renderers[0].block_type, "html");
        assert!(m.contributions.content_renderers[0].canonical);
        assert_eq!(m.contributions.prompt_segments.len(), 1);
        assert_eq!(
            m.contributions.prompt_segments[0].id,
            "internal:html-canvas:default"
        );
    }

    #[test]
    fn create_renderer_returns_canvas_for_html_block() {
        let p = HtmlCanvasPlugin;
        let r = p.create_renderer("html", ContentBlockId(0), "<p>x</p>");
        assert!(r.is_ok());
    }

    #[test]
    fn create_renderer_rejects_unknown_block_type() {
        let p = HtmlCanvasPlugin;
        let r = p.create_renderer("svg", ContentBlockId(0), "");
        assert!(matches!(
            r,
            Err(PluginError::ContentRendererNotFound(ref t)) if t == "svg"
        ));
    }
}
