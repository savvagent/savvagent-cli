//! DOM traversal that produces the ordered list of focusable elements
//! for a Blitz `BaseDocument`. Used by `HtmlCanvas::focusable_elements`.

use blitz_dom::{BaseDocument, Node, local_name};
use savvagent_plugin::{FocusableElement, Rect};

/// Walk `base` in document order, returning every focusable element's
/// `(node_id, FocusableElement)` pair. Caller stores the `node_id` for
/// later `set_focus` dispatch (to look up the Blitz node by id).
///
/// `node_id` is returned as `u32` for compatibility with the
/// `ContentRenderer` focus surface; internally Blitz uses `usize`
/// slab keys. Per the NodeId stability spike, ids are stable as long
/// as the source HTML and traversal pass remain identical.
pub fn collect(base: &BaseDocument) -> Vec<(u32, FocusableElement)> {
    let mut out = Vec::new();
    let root_id = base.root_element().id;
    walk(base, root_id, &mut out);
    out
}

fn walk(base: &BaseDocument, node_id: usize, out: &mut Vec<(u32, FocusableElement)>) {
    let node = match base.get_node(node_id) {
        Some(n) => n,
        None => return,
    };
    if is_focusable(node) {
        let rect = bounding_rect(node);
        out.push((
            node_id as u32,
            FocusableElement {
                id: format!("{node_id}"),
                bounds: rect,
            },
        ));
    }
    for child in node.children.iter().copied() {
        walk(base, child, out);
    }
}

fn is_focusable(node: &Node) -> bool {
    let element = match node.data.downcast_element() {
        Some(e) => e,
        None => return false,
    };
    let local = &element.name.local;
    if *local == *"a" {
        return element.attr(local_name!("href")).is_some();
    }
    if *local == *"button"
        || *local == *"input"
        || *local == *"select"
        || *local == *"textarea"
        || *local == *"summary"
    {
        return true;
    }
    element
        .attr(local_name!("tabindex"))
        .and_then(|v| v.parse::<i32>().ok())
        .map(|n| n >= 0)
        .unwrap_or(false)
}

fn bounding_rect(node: &Node) -> Rect {
    let l = &node.final_layout;
    let x = l.location.x.round().max(0.0) as u32;
    let y = l.location.y.round().max(0.0) as u32;
    let width = l.size.width.round().max(0.0) as u32;
    let height = l.size.height.round().max(0.0) as u32;
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use blitz_dom::{DocumentConfig, StyleThreading};
    use blitz_html::HtmlDocument;
    use blitz_traits::shell::{ColorScheme, Viewport};

    const SAMPLE: &str = r#"<!doctype html>
<html><body>
  <p>not focusable</p>
  <a href="https://example.com">link 1</a>
  <a>link without href — not focusable</a>
  <button>button</button>
  <details><summary>summary</summary><p>body</p></details>
  <input type="text">
  <div tabindex="0">tabbable div</div>
  <div tabindex="-1">explicitly skipped div</div>
</body></html>"#;

    fn parse() -> HtmlDocument {
        HtmlDocument::from_html(
            SAMPLE,
            DocumentConfig {
                base_url: None,
                net_provider: None,
                style_threading: StyleThreading::Sequential,
                viewport: Some(Viewport::new(800, 600, 1.0, ColorScheme::Light)),
                ..Default::default()
            },
        )
    }

    #[test]
    fn focusable_elements_in_document_order() {
        let mut doc = parse();
        {
            let base: &mut BaseDocument = doc.as_mut();
            base.resolve(0.0);
        }
        let base: &BaseDocument = doc.as_ref();
        let elements = collect(base);
        // Expected: link 1 (a with href), button, summary, input, tabbable div.
        // NOT: anchor-without-href, tabindex=-1 div.
        assert_eq!(elements.len(), 5, "got: {elements:#?}");
    }
}
