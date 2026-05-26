//! System-prompt segment contributions and per-slash suppression.
//!
//! A `SystemPromptSegment` is one named string the host concatenates
//! into the model's `system` field after the host's own default prompt
//! and project context. `SlashSpec::suppress_prompt_segments` lists
//! segment ids to drop for the duration of a specific slash command's
//! turn (e.g. `/commit` suppressing `internal:html-canvas:default`).
//!
//! See the inline-html-canvas spec § "Prompt contention and suppression".

/// A single contributable segment of the model's system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemPromptSegment {
    /// Stable identifier. Convention: `<plugin_id>:<segment_name>`,
    /// e.g. `"internal:html-canvas:default"`.
    pub id: String,
    /// Segment text. Concatenated verbatim after the host default
    /// prompt and project context, joined by blank lines.
    pub text: String,
}
