# Blitz NodeId stability mini-spike

Date: 2026-05-23
Blitz version: =0.3.0-alpha.4 (same as Phase 1)
Question: is Blitz NodeId stable across parses of identical HTML in
different processes? Phase 2's snapshot_state/restore_state design
keys persisted state on NodeId; if non-deterministic, fall back to
(tag, nth-of-type-path) keys.

## In-process stability

YES. The spike (`crates/_nodeid-spike`, since removed) parsed the
sample HTML twice within a single `main()` and compared the
`Vec<(NodeId as u32, format!("{:?}", node.data))>` collected via
`BaseDocument::tree().iter()`. The two vectors were equal, and the
binary printed:

```
VERDICT: NodeId is stable across two parses in the same process.
```

Note: `BaseDocument::nodes` is `pub(crate)` in 0.3.0-alpha.4, so the
spike enumerated via the public `tree()` accessor which returns
`&Slab<Node>`. `Slab::iter()` yields `(usize, &Node)` pairs whose
key is the NodeId the parser assigned. (Adapter detail; the SHAPE of
the test — parse twice, compare node enumeration — is unchanged from
the task description.)

## Cross-process stability

YES.

```
cargo run -p _nodeid-spike > /tmp/spike-a.txt
cargo run -p _nodeid-spike > /tmp/spike-b.txt
diff /tmp/spike-a.txt /tmp/spike-b.txt
# (no output)
echo $?  # 0
```

The full 72-line dump (including the redundant "first parse" and
"second parse" sections of each run) was byte-identical between the
two independent processes. Every `(NodeId, Debug(NodeData))` tuple
matched.

## Decision

**Confirmed cross-process stable.** `HtmlCanvas` serializes state
keyed by NodeId(u32). Spec's persistence section stays as written.

## Spec amendment

No amendment needed.

The Phase 2 design's "Interactive-state persistence" section in
`docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md` —
which assumes NodeId keys survive a parse-snapshot-restart-reparse-
restore round trip — is consistent with the spike's findings.

Tasks 14–16 (`state.rs CanvasState wire format`,
`HtmlCanvas::snapshot_state`, `HtmlCanvas::restore_state`) may key
on `NodeId(u32)` directly; no `(tag, nth-of-type-path)` fallback
is required for the published canvas shape.

## Caveats

- This holds for identical HTML input parsed by the same `blitz-html`
  build of `0.3.0-alpha.4`. If the source HTML changes between
  snapshot and restore (e.g. the model regenerates a slightly
  different canvas after `/resume`), NodeIds will not align and the
  restore path must tolerate missing/stale ids — that's a normal
  best-effort restore concern, not a stability issue.
- The exact-pin `=0.3.0-alpha.4` in `[workspace.dependencies]` is
  load-bearing for this guarantee. A future Blitz upgrade should
  re-run this spike (`git show <this-commit> -- crates/_nodeid-spike`
  reconstructs the throwaway crate) before relying on cross-process
  determinism continuing to hold.
