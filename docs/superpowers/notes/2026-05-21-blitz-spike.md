# Blitz embedding spike — findings

Date: 2026-05-21
Author: Phase 0 spike for the inline HTML canvas feature
Related spec: `docs/superpowers/specs/2026-05-21-inline-html-canvas-design.md`
Related plan: `docs/superpowers/plans/2026-05-21-inline-html-canvas-phase-1.md` (Task 1)

This document captures findings from the Phase 0 Blitz embedding spike. The
throwaway crate (`crates/_blitz-spike/`) used to produce these findings has
been deleted; this document and any spec amendments are the only artifacts
that survive.

## Pinned version

```toml
blitz-dom    = "=0.3.0-alpha.4"
blitz-html   = "=0.3.0-alpha.4"
blitz-paint  = "=0.3.0-alpha.4"
blitz-traits = "=0.3.0-alpha.4"
anyrender = "0.10"
anyrender_vello_cpu = "0.12"
peniko = "0.6"
```

**Pinned because:** `0.3.0-alpha.4` is the most recent published Blitz version
(crates.io, 2026-05-17, four days before this spike). The umbrella `blitz`
crate at the same version exists but adds no useful surface for headless
embedding — it just re-exports `dom`, `html`, `net`, `paint`, `shell`, and
`traits`. We depend on the building blocks directly so we don't pull in
`blitz-shell` (windowing, accesskit, winit) which we don't need.

**Alternative considered:** stable `blitz = "0.2.1"` (2025-10-08). Rejected
because the public API for `BaseDocument`/`HtmlDocument` between 0.2.x and
0.3.0-alpha.x is reportedly very different (the events surface and
`DocumentConfig`/`Viewport` types are new in 0.3); shipping against the
unstable-but-actively-maintained alpha line keeps us close to where the
upstream is going. The version is exact-pinned (`=0.3.0-alpha.4`) so a
fresh `0.3.0-alpha.5` doesn't quietly land in CI.

**MSRV impact:** Blitz declares `rust-version = "1.89.0"`. Savvagent's
`[workspace.package].rust-version` is currently `1.85`. Adopting Blitz
forces a workspace MSRV bump from 1.85 → 1.89, which should be called out
in Phase 1's Cargo.toml change + the CHANGELOG.

## What the spike did

The throwaway crate (`crates/_blitz-spike/src/main.rs`) ran end-to-end:

1. Parsed the sample HTML from the plan into an `HtmlDocument` at 800px
   viewport width.
2. Called `document.resolve(0.0)` to style + lay out the document.
3. Read the document's natural height from the root element's
   `final_layout.size.height`.
4. Painted with `anyrender::render_to_buffer::<VelloCpuImageRenderer, _>`
   driving `blitz_paint::paint_scene`.
5. Encoded the RGBA buffer to PNG via the `image` crate.
6. Probed Blitz's synthetic-event surface by:
   - Calling `BaseDocument::hit(x, y)` at a vertical band to find each
     element's screen position.
   - Calling `BaseDocument::set_hover_to(x, y)`.
   - Calling `BaseDocument::handle_dom_event(&mut DomEvent, sink)` with a
     `DomEventData::Click(BlitzPointerEvent { … })` targeted at the
     `<summary>`'s `node_id` (found via `query_selector_all`).
   - Calling `Document::handle_ui_event(UiEvent::PointerDown(…))` followed
     by `UiEvent::PointerUp(…)` at the `<summary>`'s viewport coords.
   - Re-running `resolve(0.0)` to settle any deferred state changes.

All six steps completed without panics on the first end-to-end pass; the
"natural height" path took two compile iterations (compiler told me the
exact `PointerCoords` field names and that `BlitzPointerId` has no
`Default` impl).

## Findings

### Static rendering: confirmed ✓

A clean static render works first-try with these exact calls (no patches
or forks required):

```rust
let mut document = HtmlDocument::from_html(html_source, DocumentConfig {
    base_url: None,
    net_provider: None,
    viewport: Some(Viewport::new(width, height, scale, ColorScheme::Light)),
    ..Default::default()
});
document.as_mut().resolve(0.0);

let buffer = render_to_buffer::<VelloCpuImageRenderer, _>(
    |scene| {
        scene.fill(Fill::NonZero, Default::default(), Color::WHITE,
                   Default::default(),
                   &Rect::new(0.0, 0.0, width as f64, height as f64));
        paint_scene(scene, document.as_mut(), scale as f64,
                    width, height, 0, 0);
    },
    width, height,
);
// `buffer: Vec<u8>` — RGBA, len = width * height * 4
```

The sample HTML rendered with full fidelity: the `<h1>` color, the
`.badge`'s `display: inline-block` + padding + background + border-radius,
the link in default blue with underline-on-hover (no hover in the static
render). Spike output saved to `target/spike-output.png` (800×600 RGBA).

### Pixel-buffer access: confirmed ✓

`anyrender::render_to_buffer` returns a `Vec<u8>` of length
`width * height * 4` in RGBA byte order. The `anyrender_vello_cpu` backend
is the CPU rasterizer — no GPU needed; good for the host's
"render-once-per-state-change" model and for CI.

For Phase 1 the canvas crate can return this `Vec<u8>` straight into the
WIT-portable `Frame { format: Rgba8, width, height, bytes }` type the
spec defines. `ratatui-image::Picker` will accept an RGBA `image::RgbaImage`
constructed from these bytes.

### Natural height: confirmed ✓

After `resolve(0.0)`, `document.root_element().final_layout.size.height`
returns the document's natural pixel height at the configured viewport
width. For the sample HTML at 800px width with scale=1.0 the natural
height was 241px (room to spare in the 600px viewport).

Implementation note: the root element's `final_layout` is a Taffy
`Layout` struct; `size.height` is `f32`. For the canvas, the host will:

1. Call `set_viewport(Viewport::new(width, very_tall, scale, …))` once.
2. `resolve(0.0)`.
3. Read `root_element().final_layout.size.height`.
4. Re-render at exactly that height (so the painted buffer is the doc's
   natural size, not the viewport).

This matches the spec's `ContentRenderer::render(size) -> Frame` shape;
`Frame.height` is the natural height computed from `size.width`.

### Synthetic event dispatch: NOT working as-is in 0.3.0-alpha.4 ✗

This is the load-bearing finding. The spec calls this out explicitly as
the risk that gates Phase 2; the spike confirms the risk is real.

**What works:**
- `BaseDocument::hit(x, y) -> Option<HitResult>` correctly returns the
  node under a coordinate. In the sample, hit-tests at x=120 returned
  `<span>` at y=90–100, `<summary>` at y=130–140, `<details>` at y=150–160.
- `BaseDocument::set_hover_to(x, y)` updates hover state without panicking.
- `BaseDocument::handle_dom_event(&mut DomEvent, sink)` accepts a
  `DomEvent::new(target, DomEventData::Click(BlitzPointerEvent { … }))`
  and returns without error.
- `Document::handle_ui_event(UiEvent::PointerDown(…))` /
  `UiEvent::PointerUp(…)` also accept synthetic input without error.
- `query_selector_all("summary")` finds the right node id (20 in the
  spike run).

**What does not work in 0.3.0-alpha.4:**
- Neither `handle_dom_event(Click)` nor a `handle_ui_event(PointerDown) +
  handle_ui_event(PointerUp)` sequence triggers `<summary>`'s default
  action of toggling the parent `<details>`'s `open` attribute. The
  attribute remained `Some(false)` before *and* after both attempts,
  even after a follow-up `resolve(0.0)`.
- Additionally — and unrelatedly — the static paint *renders the
  `<details>` body even when collapsed*. The spike's PNG shows the
  "Hidden by default." paragraph clearly visible despite no `open`
  attribute. So `<details>` collapse rendering is also unimplemented in
  the published headless paint at this version.

**Probable root cause:**
The `dioxus-native` shell (the consumer Blitz is designed for) implements
default actions and the `<details>` collapse-render in its own layer on
top of `BaseDocument`. The `BaseDocument` API exposes the event-dispatch
plumbing but does not run the "browser" semantics. Looking at the public
API, `BaseDocument` has `handle_dom_event` (a primitive that just walks
the DOM event-dispatch list, calls the sink for bubbled events, and
updates focus/hover state) — but there is no documented method that says
"now run any default action attached to this event," and Phase 0 found
no such side-effect empirically.

**Paths forward for Phase 2 (not blocking for Phase 1):**

1. **Host-side router on top of Blitz.** The `savvagent-canvas` crate
   tracks `<details>` and `<summary>` elements separately, intercepts
   clicks that land on a `<summary>`, and manually flips the parent
   `<details>` element's `open` attribute via Blitz's DOM-mutation API
   (set/remove attribute), then calls `resolve()` again. This is roughly
   what a userland JS library would do in a real browser if `<details>`
   support were missing. Same pattern works for link follows
   (intercept anchor clicks → emit `Effect::OpenUrl`).

2. **Wait / contribute upstream.** Blitz's 0.3.0-alpha line is moving
   fast (alpha.1 → alpha.4 in under a month). Default actions for the
   most common elements may land before Phase 2 ships. If Phase 2 starts
   when alpha.5+ has been published, re-run this spike on the new
   version first.

3. **Investigate `blitz-shell` further.** The shell crate (which we
   excluded from the spike to stay headless) may have a publicly-callable
   helper that wraps the necessary steps. Worth a 30-minute look at the
   start of Phase 2 implementation.

4. **Render-side fix for `<details>` collapse.** Independent of events:
   the headless paint also doesn't respect `<details open>`. For Phase 1
   the model can be prompted to avoid `<details>` initially, or the
   subset validator can transform `<details>` → `<div>` with a leading
   `▸ <summary text>`. For Phase 2 this naturally resolves once the host
   router toggles the attribute.

The spec already anticipates this — its "Approach risks" section says
exactly *"if it's rough, we ship Phase 1 (static rendering) on schedule
and the spike's findings shape whether Phase 2 needs a savvagent-side
event router."* The answer is "yes, Phase 2 needs a host-side event
router on top of Blitz."

### CSS subset coverage

What the spike's sample HTML exercised, and what worked:

| Property / feature | Status |
|---|---|
| `font-family: sans-serif` | ✓ rendered (system sans-serif) |
| `margin` (block) | ✓ |
| `color` (hex) | ✓ |
| `display: inline-block` | ✓ |
| `padding` (4-value) | ✓ |
| `background` (hex) | ✓ |
| `border-radius: 12px` | ✓ rounded corners visible |
| `a:hover { text-decoration: underline }` | n/a in static (no hover) |
| `details > summary { cursor: pointer }` | irrelevant (no cursor in headless) |
| `<details>` rendering | ✗ body painted even when `open` absent |
| `<details>` toggle on click | ✗ default action not run |

Not exercised in the spike (deferred to canvas-crate work):
- `display: flex` / `grid` (the article example would have these; their
  Blitz support is documented as working but unconfirmed by this spike)
- `box-shadow`, `opacity`, `transform`, `transition`
- Forms (`<input>`, `<select>`, etc.) — likely the same "synthetic event
  doesn't update DOM" pattern.

The subset validator in `savvagent-canvas` should at minimum warn for
`<details>` and form inputs until Phase 2 lands.

## Decision

**Spec: amended.**

The spec's *core* approach is confirmed — Blitz does cleanly support the
static-rendering path that Phase 1 requires. The interactive (Phase 2)
risks are now concrete instead of speculative, and the spec's "Approach
risks" + "Risk register" sections are updated to reflect what the spike
actually found.

Specifically:

1. The "Approach risks" bullet about eventing is rewritten to say
   "confirmed at 0.3.0-alpha.4: synthetic events accepted, default
   actions not run" — and the mitigation is identified concretely as
   "host-side router for `<details>`, link follow, form submission."
2. A new entry in the "Approach risks" section calls out that
   `<details>` body also isn't *rendered* respecting `open` state in the
   headless paint; Phase 1 either prompts the model to avoid `<details>`
   or transforms it server-side. Default: prompt the model to use it
   sparingly until Phase 2 lands.
3. The "Open questions" section gains a pointer to this notes file.
4. Phase 2's eventing trait surface (`ContentRenderer::dispatch`) stays
   as designed; the host-side router lives *inside* the
   `savvagent-canvas` crate's `HtmlCanvas::dispatch` impl, so the trait
   surface is unaffected.

## Phase 2 risk update

**Verdict: needs router layer.** Phase 2 ships with a host-side router
inside `savvagent-canvas::HtmlCanvas::dispatch` that intercepts:

- Clicks landing on `<summary>` elements → flip parent `<details>`
  `open` attribute → re-resolve.
- Clicks landing on `<a href>` elements → emit `Effect::OpenUrl`
  (no event passed down to Blitz).
- Form submission (`<button type="submit">` / Enter in `<input>`) →
  emit `Effect::OpenUrl` with synthesized query-string.

The `ContentRenderer` trait surface defined in the spec is unaffected
by this — the router is an implementation detail of one method.

If Blitz publishes alpha.5+ with default-action support before Phase 2
starts, the router becomes a no-op pass-through; we lose nothing by
landing it.

## Spike artifacts (pre-teardown)

For reference if anyone re-runs the spike:

- Throwaway crate: `crates/_blitz-spike/` (deleted at end of Task 1)
- PNG output: `target/spike-output.png` (also deleted by `cargo clean`)
- Sample HTML: inlined as `SAMPLE_HTML` in `crates/_blitz-spike/src/main.rs`

The full spike source has been preserved in this commit's git history via
the pre-teardown checkpoint — running `git log -p -- crates/_blitz-spike`
on the branch this commit lands on will not show it (it was deleted in
the same commit), but the spike's main.rs is reproducible from the
description above plus the screenshot.rs upstream example
(<https://github.com/DioxusLabs/blitz/blob/main/examples/screenshot.rs>).

## Open questions resolved

- "Crate version of Blitz to pin": `0.3.0-alpha.4` of `blitz-dom` /
  `blitz-html` / `blitz-paint` / `blitz-traits` + `anyrender 0.10` +
  `anyrender_vello_cpu 0.12` + `peniko 0.6`.

## Open questions raised

- Will Blitz `0.3.0-alpha.5+` change the `DomEvent`/`UiEvent` API shape
  again? The transition from 0.2.x to 0.3.0-alpha.x was substantial.
  Mitigation: exact-pin (`=0.3.0-alpha.4`) and gate Phase 2 start on a
  re-run of this spike against whatever's current then.
- Does `blitz-shell` expose a "run default action for this event"
  helper that we could call from headless code? Worth 30 minutes of
  investigation at the start of Phase 2.
- Forms: does `<input>` accept synthetic typed text via
  `UiEvent::Ime(BlitzImeEvent)` and reflect it in `value`? Not exercised
  in this spike; first thing to check in Phase 2.
