//! Default system prompt segment text for internal:html-canvas.

/// The default segment text. Kept in its own file so it's easy to
/// review and i18n later.
pub const DEFAULT_PROMPT_TEXT: &str = "\
When responding to the user with a structured document — plan, spec, \
status update, design review, comparison table, anything where visual \
hierarchy and scannability matter — prefer HTML over markdown. Wrap \
the HTML in a ```html-canvas fenced block. The user's terminal renders \
it inline as a document.\n\
\n\
For code samples, terse replies, error messages, or output destined for \
another system (commit messages, PR comments, files on disk), use plain \
text or markdown — those are not rendered as canvases.\n\
\n\
Supported tags: <h1>-<h6>, <p>, <ul>, <ol>, <li>, <dl>, <dt>, <dd>, \
<table>, <thead>, <tbody>, <tr>, <th>, <td>, <pre>, <code>, <a>, <em>, \
<strong>, <mark>, <kbd>, <details>, <summary>, <blockquote>, <hr>, \
<section>, <header>, <footer>, <figure>, <figcaption>, <img> (data: URIs only), \
<svg>.\n\
\n\
Use a <style> block in the document head; do not link external \
stylesheets. Do not include <script> tags. Do not reference external \
fonts. Use only data: URIs for images. Do not use <iframe>, <video>, \
<audio>, <embed>, <object>, or <canvas>.\
";

/// Stable id for the segment. Matches the convention
/// `<plugin_id>:<segment_name>`.
pub const DEFAULT_PROMPT_ID: &str = "internal:html-canvas:default";
