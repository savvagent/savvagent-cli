//! Subset validator: walks parsed HTML and emits tracing warnings for
//! elements or attributes outside the savvagent-canvas supported set.
//!
//! Not a render error — Blitz renders what it can; the validator's
//! purpose is to surface "you used <iframe>; that's not in the
//! subset" warnings during development.

/// Validate `source` and emit `tracing::warn!` for anything outside
/// the documented subset. Returns the count of warnings emitted.
pub fn validate(source: &str) -> usize {
    let mut warnings = 0;
    let lower = source.to_ascii_lowercase();

    // Light tokenizer (not full HTML parsing): just look for the
    // bracketed tag names. Good enough for surfacing the obvious
    // violations like <script>, <iframe>, <video>, <audio>, <embed>,
    // <object>, <canvas>, external <link rel="stylesheet">, etc.
    for &(tag, msg) in EXCLUDED_TAGS {
        let needle = format!("<{}", tag);
        if lower.contains(&needle) {
            warnings += 1;
            tracing::warn!(tag, "savvagent-canvas: {}", msg,);
        }
    }

    if source.contains("rel=\"stylesheet\"") || source.contains("rel='stylesheet'") {
        warnings += 1;
        tracing::warn!(
            "savvagent-canvas: external stylesheets are not loaded; \
             use a <style> block instead"
        );
    }

    warnings
}

/// `(tag_name, warning_message)` pairs for elements outside the subset.
///
/// `<details>` is included with a distinct message: Blitz 0.3.0-alpha.4
/// paints `<details>` regardless of the `open` attribute, so content
/// that should be hidden is always visible. This is a known Phase 1
/// limitation flagged in the spike notes.
const EXCLUDED_TAGS: &[(&str, &str)] = &[
    (
        "script",
        "<script> is outside the subset; will not render as intended",
    ),
    (
        "iframe",
        "<iframe> is outside the subset; will not render as intended",
    ),
    (
        "object",
        "<object> is outside the subset; will not render as intended",
    ),
    (
        "embed",
        "<embed> is outside the subset; will not render as intended",
    ),
    (
        "video",
        "<video> is outside the subset; will not render as intended",
    ),
    (
        "audio",
        "<audio> is outside the subset; will not render as intended",
    ),
    (
        "canvas",
        "<canvas> (HTML canvas element) is outside the subset; will not render as intended",
    ),
    (
        "details",
        "<details> paints regardless of the `open` attribute in Blitz \
         0.3.0-alpha.4 (Phase 1 limitation); consider alternatives",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_html_emits_no_warnings() {
        let n = validate("<!doctype html><body><h1>x</h1></body>");
        assert_eq!(n, 0);
    }

    #[test]
    fn script_tag_warns() {
        let n = validate("<!doctype html><body><script>alert(1)</script></body>");
        assert_eq!(n, 1);
    }

    #[test]
    fn external_stylesheet_warns() {
        let n = validate(
            "<!doctype html><head>\
             <link rel=\"stylesheet\" href=\"x.css\"></head><body></body>",
        );
        assert_eq!(n, 1);
    }

    #[test]
    fn multiple_violations_count_independently() {
        let n = validate("<!doctype html><body><script>x</script><iframe src='y'></iframe></body>");
        assert_eq!(n, 2);
    }

    #[test]
    fn details_tag_warns() {
        let n = validate(
            "<!doctype html><body>\
             <details><summary>Title</summary><p>Hidden content</p></details>\
             </body>",
        );
        assert_eq!(n, 1);
    }
}
