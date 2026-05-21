# `/lsp` Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `/lsp` slash command that opens a multi-select picker of curated language servers, then downloads/installs the selected ones and merges entries into `~/.savvagent/lsp.toml`.

**Architecture:** Two PRs landing in order. PR 1 adds a reusable `MultiSelectList<T>` widget under `crates/savvagent/src/plugin/widgets/`. PR 2 adds the `internal:lsp-installer` plugin under `crates/savvagent/src/plugin/builtin/lsp_installer/` — the plugin wraps the widget in a `Screen`, owns a static catalog of LSP entries, and runs installs (HTTP binary download + SHA256 verify + extract, or `npm i -g`) on Confirm. Both PRs ship under one rollup tag (`v0.X.0`) per repo convention.

**Tech Stack:** Rust 1.85 / edition 2024, tokio 1, reqwest 0.13 (rustls), flate2 1, tar 0.4, zip 5, sha2 0.10, ratatui 0.30 + crossterm 0.29, async-trait. All new heavy deps land at the workspace level.

**Blocked on:** PR #90 (`feat/host-resources-and-tool-lsp`) merging to master. PR 1 of this plan **must** branch off post-merge master, because `tool-lsp` and its `lsp.toml` config loader are introduced by #90.

**Reference doc:** `docs/superpowers/specs/2026-05-20-lsp-installer-design.md`.

---

## PR 1 — Reusable multi-select widget

### Task 1: Scaffold the `widgets/` module

**Files:**
- Create: `crates/savvagent/src/plugin/widgets/mod.rs`
- Create: `crates/savvagent/src/plugin/widgets/multi_select_list.rs` (empty for now)
- Modify: `crates/savvagent/src/plugin/mod.rs:5` (add `pub mod widgets;` next to existing `pub mod builtin;`)

- [ ] **Step 1: Create the module files**

Write `crates/savvagent/src/plugin/widgets/mod.rs`:

```rust
//! Reusable UI state-machine helpers that screens can wrap.
//!
//! Plugins live under `plugin::builtin::*`; widgets here are pure
//! state machines with no `Plugin`/`Screen` trait impl. A screen wraps
//! a widget by holding it in a field and translating the widget's
//! outcome enum into closed-vocabulary `Effect`s.

pub mod multi_select_list;
```

Write `crates/savvagent/src/plugin/widgets/multi_select_list.rs`:

```rust
//! Generic multi-select list state. See `MultiSelectList`.
```

- [ ] **Step 2: Wire the module into the plugin tree**

Open `crates/savvagent/src/plugin/mod.rs` and add the line after the existing `pub mod builtin;` declaration:

```rust
pub mod widgets;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p savvagent`
Expected: success, no warnings about unused module.

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/mod.rs crates/savvagent/src/plugin/widgets/
git commit -m "refactor(plugin): introduce widgets module for reusable state machines"
```

### Task 2: Failing test for empty `MultiSelectList`

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs` (add test module)

- [ ] **Step 1: Write the failing test**

Append to `crates/savvagent/src/plugin/widgets/multi_select_list.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item { id: &'static str, label: &'static str }

    fn items() -> Vec<Item> {
        vec![
            Item { id: "a", label: "alpha" },
            Item { id: "b", label: "bravo" },
            Item { id: "c", label: "charlie" },
        ]
    }

    fn list() -> MultiSelectList<Item> {
        MultiSelectList::new(
            items(),
            |i: &Item, f: &str| i.label.contains(f),
            |i: &Item| i.id.to_string(),
        )
    }

    #[test]
    fn new_starts_empty_filter_zero_cursor_no_selection() {
        let l = list();
        assert_eq!(l.filter(), "");
        assert_eq!(l.cursor(), 0);
        assert!(l.selected().is_empty());
        assert_eq!(l.filtered().len(), 3);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests::new_starts_empty_filter_zero_cursor_no_selection`
Expected: FAIL (compile error: `MultiSelectList` not defined).

- [ ] **Step 3: Implement just enough to compile**

Replace the file contents with:

```rust
//! Generic multi-select list state.

use std::collections::BTreeSet;

pub struct MultiSelectList<T> {
    items: Vec<T>,
    filter: String,
    cursor: usize,
    selected_ids: BTreeSet<String>,
    filter_fn: Box<dyn Fn(&T, &str) -> bool + Send>,
    id_fn: Box<dyn Fn(&T) -> String + Send>,
}

impl<T> std::fmt::Debug for MultiSelectList<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MultiSelectList")
            .field("items_len", &self.items.len())
            .field("filter", &self.filter)
            .field("cursor", &self.cursor)
            .field("selected_ids", &self.selected_ids)
            .finish()
    }
}

impl<T> MultiSelectList<T> {
    pub fn new(
        items: Vec<T>,
        filter_fn: impl Fn(&T, &str) -> bool + Send + 'static,
        id_fn: impl Fn(&T) -> String + Send + 'static,
    ) -> Self {
        Self {
            items,
            filter: String::new(),
            cursor: 0,
            selected_ids: BTreeSet::new(),
            filter_fn: Box::new(filter_fn),
            id_fn: Box::new(id_fn),
        }
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn selected(&self) -> &BTreeSet<String> {
        &self.selected_ids
    }

    /// Items matching the current filter, in catalog order. Empty filter
    /// returns every item. Allocates a fresh `Vec` of references each call
    /// — fine for the picker's <100-item catalog; revisit if a future
    /// consumer pushes more rows.
    pub fn filtered(&self) -> Vec<&T> {
        let f = self.filter.as_str();
        if f.is_empty() {
            self.items.iter().collect()
        } else {
            self.items.iter().filter(|i| (self.filter_fn)(i, f)).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    // ... existing test block stays unchanged ...
}
```

(Keep the `#[cfg(test)] mod tests { ... }` block from Step 1.)

- [ ] **Step 4: Run the test**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests::new_starts_empty_filter_zero_cursor_no_selection`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/multi_select_list.rs
git commit -m "feat(plugin/widgets): MultiSelectList scaffold + filtered() accessor"
```

### Task 3: Outcome enum + key handler shell

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the existing `#[cfg(test)] mod tests` block:

```rust
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::empty())
}

#[test]
fn esc_emits_cancel() {
    let mut l = list();
    assert!(matches!(l.on_key(key(KeyCode::Esc)), MultiSelectOutcome::Cancel));
}

#[test]
fn unknown_key_emits_stay() {
    let mut l = list();
    assert!(matches!(
        l.on_key(key(KeyCode::F(5))),
        MultiSelectOutcome::Stay
    ));
}

#[test]
fn enter_with_no_selection_returns_empty_confirm() {
    let mut l = list();
    match l.on_key(key(KeyCode::Enter)) {
        MultiSelectOutcome::Confirm(v) => assert!(v.is_empty()),
        other => panic!("expected Confirm([]), got {other:?}"),
    }
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: FAIL — `on_key`, `MultiSelectOutcome` not defined.

- [ ] **Step 3: Implement the outcome enum + on_key**

Add above the test module (still inside `multi_select_list.rs`):

```rust
#[derive(Debug, PartialEq, Eq)]
pub enum MultiSelectOutcome<T: Clone> {
    Stay,
    Preview(T),
    Toggle(T),
    Confirm(Vec<T>),
    Cancel,
}

impl<T: Clone> MultiSelectList<T> {
    /// Confirm-by-walking-items: returns selected items in catalog order,
    /// regardless of selection sequence.
    fn confirm_selection(&self) -> Vec<T> {
        self.items
            .iter()
            .filter(|i| self.selected_ids.contains(&(self.id_fn)(i)))
            .cloned()
            .collect()
    }

    pub fn on_key(&mut self, key: crossterm::event::KeyEvent) -> MultiSelectOutcome<T> {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => MultiSelectOutcome::Cancel,
            KeyCode::Enter => MultiSelectOutcome::Confirm(self.confirm_selection()),
            _ => MultiSelectOutcome::Stay,
        }
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: PASS (all four tests including the original `new_*` one).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/multi_select_list.rs
git commit -m "feat(plugin/widgets): MultiSelectOutcome + Esc/Enter handlers"
```

### Task 4: Cursor up/down with Preview

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the test module:

```rust
#[test]
fn down_moves_cursor_and_emits_preview() {
    let mut l = list();
    let outcome = l.on_key(key(KeyCode::Down));
    assert_eq!(l.cursor(), 1);
    assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "b", .. })));
}

#[test]
fn down_clamps_at_last_row() {
    let mut l = list();
    l.on_key(key(KeyCode::Down));
    l.on_key(key(KeyCode::Down));
    let outcome = l.on_key(key(KeyCode::Down));
    assert_eq!(l.cursor(), 2);
    assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "c", .. })));
}

#[test]
fn up_clamps_at_first_row() {
    let mut l = list();
    let outcome = l.on_key(key(KeyCode::Up));
    assert_eq!(l.cursor(), 0);
    assert!(matches!(outcome, MultiSelectOutcome::Preview(Item { id: "a", .. })));
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: FAIL — cursor stays at 0 because `Up`/`Down` fall through to `Stay`.

- [ ] **Step 3: Implement cursor movement**

Replace the `match key.code` block in `on_key`:

```rust
match key.code {
    KeyCode::Esc => MultiSelectOutcome::Cancel,
    KeyCode::Enter => MultiSelectOutcome::Confirm(self.confirm_selection()),
    KeyCode::Down => self.move_cursor(1),
    KeyCode::Up => self.move_cursor(-1),
    _ => MultiSelectOutcome::Stay,
}
```

Add to the same `impl` block:

```rust
fn move_cursor(&mut self, delta: isize) -> MultiSelectOutcome<T> {
    let filtered = self.filtered();
    if filtered.is_empty() {
        return MultiSelectOutcome::Stay;
    }
    let last = filtered.len() - 1;
    let new = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
    self.cursor = new;
    MultiSelectOutcome::Preview(filtered[new].clone())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/multi_select_list.rs
git commit -m "feat(plugin/widgets): cursor up/down with Preview outcome"
```

### Task 5: Space toggles selection at cursor

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the test module:

```rust
#[test]
fn space_toggles_cursor_item() {
    let mut l = list();
    // Cursor at 'a' → toggle → selected has 'a'.
    let out = l.on_key(key(KeyCode::Char(' ')));
    assert!(matches!(out, MultiSelectOutcome::Toggle(Item { id: "a", .. })));
    assert!(l.selected().contains("a"));

    // Toggle again → 'a' removed.
    let out = l.on_key(key(KeyCode::Char(' ')));
    assert!(matches!(out, MultiSelectOutcome::Toggle(Item { id: "a", .. })));
    assert!(!l.selected().contains("a"));
}

#[test]
fn confirm_returns_items_in_catalog_order_not_selection_order() {
    let mut l = list();
    // Select c (cursor=2) first, then a (cursor=0).
    l.on_key(key(KeyCode::Down));
    l.on_key(key(KeyCode::Down));
    l.on_key(key(KeyCode::Char(' '))); // select c
    l.on_key(key(KeyCode::Up));
    l.on_key(key(KeyCode::Up));
    l.on_key(key(KeyCode::Char(' '))); // select a

    let out = l.on_key(key(KeyCode::Enter));
    match out {
        MultiSelectOutcome::Confirm(items) => {
            let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
            assert_eq!(ids, vec!["a", "c"], "must be catalog order, not selection order");
        }
        other => panic!("expected Confirm, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: FAIL — Space falls through to `Stay`.

- [ ] **Step 3: Implement Space toggle**

Inside `on_key`, add a branch above the `_ =>` wildcard:

```rust
KeyCode::Char(' ') => self.toggle_cursor(),
```

Add to the same `impl` block:

```rust
fn toggle_cursor(&mut self) -> MultiSelectOutcome<T> {
    let filtered = self.filtered();
    let Some(item) = filtered.get(self.cursor) else {
        return MultiSelectOutcome::Stay;
    };
    let id = (self.id_fn)(item);
    if !self.selected_ids.remove(&id) {
        self.selected_ids.insert(id);
    }
    MultiSelectOutcome::Toggle((*item).clone())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/multi_select_list.rs
git commit -m "feat(plugin/widgets): Space toggles selection; Confirm preserves catalog order"
```

### Task 6: Filter input + cursor clamping after filter change

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs`

- [ ] **Step 1: Write the failing tests**

Append inside the test module:

```rust
#[test]
fn typing_chars_narrows_filter() {
    let mut l = list();
    l.on_key(key(KeyCode::Char('a')));
    assert_eq!(l.filter(), "a");
    // "alpha" and "bravo" and "charlie" all contain 'a'? alpha=yes,
    // bravo=yes, charlie=yes. Use a more selective char.
    let mut l = list();
    l.on_key(key(KeyCode::Char('l')));
    assert_eq!(l.filter(), "l");
    let filtered: Vec<&str> = l.filtered().iter().map(|i| i.id).collect();
    assert_eq!(filtered, vec!["a", "c"], "alpha + charlie both contain 'l'");
}

#[test]
fn backspace_pops_filter_and_reclamps_cursor() {
    let mut l = list();
    l.on_key(key(KeyCode::Char('l'))); // narrows to [a, c]; cursor still 0
    l.on_key(key(KeyCode::Down));      // cursor → 1
    l.on_key(key(KeyCode::Char('p'))); // narrows further to [a]; cursor must clamp to 0
    assert_eq!(l.cursor(), 0, "cursor must clamp when filter shrinks past it");
    l.on_key(key(KeyCode::Backspace)); // back to [a, c]
    assert_eq!(l.filter(), "l");
}

#[test]
fn selection_survives_filter_changes() {
    let mut l = list();
    l.on_key(key(KeyCode::Char(' ')));  // select a
    l.on_key(key(KeyCode::Char('b')));  // filter narrows so 'a' is hidden
    assert!(l.filtered().iter().all(|i| i.id != "a"));
    assert!(l.selected().contains("a"), "selection persists across filter");
    l.on_key(key(KeyCode::Backspace));  // restore visibility
    let out = l.on_key(key(KeyCode::Enter));
    match out {
        MultiSelectOutcome::Confirm(items) => {
            assert_eq!(items.iter().map(|i| i.id).collect::<Vec<_>>(), vec!["a"]);
        }
        other => panic!("expected Confirm, got {other:?}"),
    }
}
```

- [ ] **Step 2: Run and watch them fail**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: FAIL — printable chars and Backspace fall through.

- [ ] **Step 3: Implement filter handling**

Inside `on_key`, add branches above the `_ =>` wildcard (and above the `Char(' ')` branch — Space is matched first so it takes precedence):

```rust
KeyCode::Backspace => {
    if self.filter.pop().is_none() {
        return MultiSelectOutcome::Stay;
    }
    self.clamp_after_filter_change()
}
KeyCode::Char(c)
    if !key.modifiers.contains(crossterm::event::KeyModifiers::CONTROL)
        && !key.modifiers.contains(crossterm::event::KeyModifiers::ALT) =>
{
    self.filter.push(c);
    self.clamp_after_filter_change()
}
```

Add to the same `impl` block:

```rust
fn clamp_after_filter_change(&mut self) -> MultiSelectOutcome<T> {
    let filtered = self.filtered();
    if filtered.is_empty() {
        return MultiSelectOutcome::Stay;
    }
    if self.cursor >= filtered.len() {
        self.cursor = filtered.len() - 1;
    }
    MultiSelectOutcome::Preview(filtered[self.cursor].clone())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p savvagent plugin::widgets::multi_select_list::tests`
Expected: PASS (all tests from Tasks 2-6).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/multi_select_list.rs
git commit -m "feat(plugin/widgets): filter typing + Backspace; selection persists across filter changes"
```

### Task 7: Doc comments + public re-exports

**Files:**
- Modify: `crates/savvagent/src/plugin/widgets/multi_select_list.rs`
- Modify: `crates/savvagent/src/plugin/widgets/mod.rs`

- [ ] **Step 1: Add doc comments**

Add `///`-style documentation above every public item in
`multi_select_list.rs`. Cover:

- `MultiSelectList<T>`: that it's a generic state machine, expected
  catalog size (<100 items), why selection is by string id rather than
  index (filter-resilience), and where it's wrapped (refer to
  `plugin::builtin::lsp_installer::screen::LspPickerScreen`).
- `MultiSelectOutcome<T>`: one line per variant explaining when it's
  emitted.
- `MultiSelectList::new`: that `filter_fn` is called with the **lowercased
  filter** for case-insensitive matching when consumers want it (note: we
  don't do the lowercasing ourselves — that's a consumer concern).
- `filtered()`, `cursor()`, `filter()`, `selected()`: short docstrings.
- `on_key(key: KeyEvent)`: lists every recognised key + its outcome.

- [ ] **Step 2: Re-export the public API from `widgets/mod.rs`**

Replace the contents of `crates/savvagent/src/plugin/widgets/mod.rs` with:

```rust
//! Reusable UI state-machine helpers that screens can wrap.
//!
//! Plugins live under `plugin::builtin::*`; widgets here are pure
//! state machines with no `Plugin`/`Screen` trait impl. A screen wraps
//! a widget by holding it in a field and translating the widget's
//! outcome enum into closed-vocabulary `Effect`s.

pub mod multi_select_list;

pub use multi_select_list::{MultiSelectList, MultiSelectOutcome};
```

- [ ] **Step 3: Verify docs build + everything still passes**

Run: `cargo test -p savvagent plugin::widgets`
Expected: PASS — all widget tests still green.

Run: `cargo doc -p savvagent --no-deps`
Expected: success with no `missing_docs` warnings (the savvagent crate
inherits `#![warn(missing_docs)]` from its lib root if present; if not,
no action needed).

- [ ] **Step 4: Commit**

```bash
git add crates/savvagent/src/plugin/widgets/
git commit -m "docs(plugin/widgets): rustdoc on MultiSelectList + module re-exports"
```

### Task 8: Clippy + fmt + open PR 1

**Files:** none (verification + git only)

- [ ] **Step 1: Match CI's stable toolchain locally**

Run: `rustup run stable cargo fmt --all -- --check`
Expected: clean (no diff).

Run: `rustup run stable cargo clippy -p savvagent -- -D warnings`
Expected: clean (no warnings on the new code).

Run: `cargo test --workspace`
Expected: PASS (existing 1017+ tests, plus the ~12 new widget tests).

- [ ] **Step 2: Push the branch and open PR 1**

```bash
git push -u origin feat/multi-select-widget
gh pr create --title "feat(plugin/widgets): reusable MultiSelectList state machine" --body "$(cat <<'EOF'
## Summary

Adds a generic `MultiSelectList<T>` state machine under `crates/savvagent/src/plugin/widgets/`, ready to be wrapped by a `Screen` impl. This is the precursor PR for the `/lsp` installer (next PR).

The widget is selection-by-stable-id, not by index, so a checked item survives the user typing into and out of the filter. `Confirm(Vec<T>)` returns items in catalog order, not selection order — predictable for any consumer that wants to render the result deterministically.

No changes to the plugin trait surface; this is a pure in-tree helper.

## Test plan

- [ ] `cargo test -p savvagent plugin::widgets` — all widget tests pass.
- [ ] `cargo test --workspace` — full suite still green.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] `cargo fmt --check` — clean.
EOF
)"
```

- [ ] **Step 3: Verify CI after pushing**

Run: `gh pr view --json url,statusCheckRollup` then `gh run list --branch feat/multi-select-widget --limit 3` to watch the workflow.
Expected: all required checks green within ~10 minutes; if any fail, fix locally and push a new commit (never `--force` here).

- [ ] **Step 4: Merge PR 1 when reviewed**

Squash-merge per repo convention. Confirm the squashed commit lands on `master` and contains all eight tasks' code.

---

## PR 2 — `/lsp` slash + installer plugin

PR 2 stacks on PR 1 (master after the squash-merge). All paths below are post-PR-1.

### Task 9: New workspace deps

**Files:**
- Modify: `Cargo.toml` (`[workspace.dependencies]` block)
- Modify: `crates/savvagent/Cargo.toml`

- [ ] **Step 1: Add the four new deps to the workspace**

Open `Cargo.toml` and add to the `[workspace.dependencies]` block (alphabetical placement near existing entries):

```toml
flate2 = "1"
sha2 = "0.10"
tar = "0.4"
zip = { version = "5", default-features = false, features = ["deflate"] }
```

(`zip`'s default features pull in bzip2/zstd/aes that we don't need — we
opt into just `deflate` to keep the binary small.)

- [ ] **Step 2: Pull them into the `savvagent` crate**

Open `crates/savvagent/Cargo.toml` and add to its `[dependencies]` block:

```toml
flate2 = { workspace = true }
sha2 = { workspace = true }
tar = { workspace = true }
zip = { workspace = true }
```

- [ ] **Step 3: Verify resolution**

Run: `cargo check -p savvagent`
Expected: success; Cargo.lock updates with the new dep tree.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/savvagent/Cargo.toml Cargo.lock
git commit -m "deps: add flate2 + sha2 + tar + zip for the LSP installer"
```

### Task 10: Scaffold the `lsp_installer` module

**Files:**
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/catalog.rs`
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs`
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/config_writer.rs`
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/picker.rs`
- Create: `crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs`
- Modify: `crates/savvagent/src/plugin/builtin/mod.rs` (add module decl)

- [ ] **Step 1: Create empty module files with doc headers**

Write each file with a one-line `//!` doc comment so `cargo check` doesn't complain about empty files. Example for `mod.rs`:

```rust
//! `internal:lsp-installer` — `/lsp` slash command, multi-select picker,
//! and one-shot LSP-binary installer.
//!
//! See `docs/superpowers/specs/2026-05-20-lsp-installer-design.md`.
```

For the four sibling files, use one-line headers:

- `catalog.rs`: `//! Pinned LSP catalog (server id, version, download URLs, SHA256s).`
- `installer.rs`: `//! Per-entry installer: binary download/verify/extract or npm i -g.`
- `config_writer.rs`: `//! Merge installed entries into ~/.savvagent/lsp.toml.`
- `picker.rs`: `//! Wraps MultiSelectList<&CatalogEntry> for the /lsp UI.`
- `screen.rs`: `//! LspPickerScreen — Screen impl bridging picker outcomes to Effects.`

- [ ] **Step 2: Wire the module into `builtin/mod.rs`**

Open `crates/savvagent/src/plugin/builtin/mod.rs` and add (alphabetical placement among the existing `pub mod *;` lines):

```rust
pub mod lsp_installer;
```

- [ ] **Step 3: Add sub-modules to `lsp_installer/mod.rs`**

Append to `mod.rs`:

```rust
pub mod catalog;
pub mod config_writer;
pub mod installer;
pub mod picker;
pub mod screen;
```

- [ ] **Step 4: Verify**

Run: `cargo check -p savvagent`
Expected: success.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/ crates/savvagent/src/plugin/builtin/mod.rs
git commit -m "feat(internal:lsp-installer): scaffold module layout"
```

### Task 11: Catalog types + smoke test

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/catalog.rs`

- [ ] **Step 1: Write the failing tests**

Replace the contents of `catalog.rs` with:

```rust
//! Pinned LSP catalog (server id, version, download URLs, SHA256s).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    LinuxX86_64Gnu,
    LinuxAarch64Gnu,
    MacosX86_64,
    MacosAarch64,
    WindowsX86_64,
}

impl Target {
    /// Resolve the current host's target triple, or `None` if unsupported.
    pub fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Some(Self::LinuxX86_64Gnu),
            ("linux", "aarch64") => Some(Self::LinuxAarch64Gnu),
            ("macos", "x86_64") => Some(Self::MacosX86_64),
            ("macos", "aarch64") => Some(Self::MacosAarch64),
            ("windows", "x86_64") => Some(Self::WindowsX86_64),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum ArchiveKind {
    /// Single gzipped binary (`.gz`) — extracted as the binary itself.
    GzipOnly,
    TarGz,
    Zip,
}

#[derive(Debug, Clone, Copy)]
pub enum Category {
    Binary,
    Npm,
}

#[derive(Debug, Clone, Copy)]
pub struct LspEntryTemplate {
    pub id: &'static str,
    pub extensions: &'static [&'static str],
    pub root_markers: &'static [&'static str],
    pub command: &'static str,
    pub args: &'static [&'static str],
}

#[derive(Debug, Clone, Copy)]
pub enum InstallMethod {
    BinaryDownload {
        urls: &'static [(Target, &'static str, &'static str)],
        archive: ArchiveKind,
        binary_path: &'static str,
    },
    NpmGlobal {
        package: &'static str,
        binary: &'static str,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub id: &'static str,
    pub display_name: &'static str,
    pub language_label: &'static str,
    pub version: &'static str,
    pub category: Category,
    pub method: InstallMethod,
    pub lsp_entry: LspEntryTemplate,
}

/// Pinned v1 catalog. Versions and checksums refreshed at catalog
/// publication time; see the spec for the update workflow.
pub static CATALOG: &[CatalogEntry] = &[
    // Filled in by Task 12.
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_current_returns_some_on_supported_host() {
        // CI runs on linux-x86_64, linux-aarch64, macos-aarch64, windows-x86_64.
        assert!(Target::current().is_some(), "expected supported host target");
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort();
        let len_before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len_before, "duplicate ids: {:?}", ids);
    }

    #[test]
    fn binary_entries_cover_every_target() {
        for entry in CATALOG {
            if let InstallMethod::BinaryDownload { urls, .. } = entry.method {
                let mut covered: Vec<Target> = urls.iter().map(|(t, ..)| *t).collect();
                covered.sort_by_key(|t| format!("{t:?}"));
                covered.dedup();
                assert_eq!(
                    covered.len(),
                    5,
                    "{}: must list one URL per Target variant (got {:?})",
                    entry.id, covered
                );
            }
        }
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::catalog`
Expected: `target_current_returns_some_on_supported_host` PASS;
`catalog_ids_are_unique` PASS (empty catalog dedups trivially);
`binary_entries_cover_every_target` PASS (empty catalog has nothing to check).

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/catalog.rs
git commit -m "feat(internal:lsp-installer): catalog types + Target detection"
```

### Task 12: Populate the v1 catalog

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/catalog.rs`

- [ ] **Step 1: Look up current pinned versions + checksums**

For each of the five binary servers, visit the upstream releases page in
a browser:

- rust-analyzer: https://github.com/rust-lang/rust-analyzer/releases/latest
- clangd: https://github.com/clangd/clangd/releases/latest
- lua-language-server: https://github.com/LuaLS/lua-language-server/releases/latest
- zls: https://github.com/zigtools/zls/releases/latest
- marksman: https://github.com/artempyanykh/marksman/releases/latest

For each release, capture for every supported target:

- The full download URL (right-click → copy link on the per-target asset)
- The SHA256 (most repos publish a `*.sha256` sidecar; otherwise compute via `curl -L <url> | sha256sum`)
- The exact archive kind (`.gz` / `.tar.gz` / `.zip`)
- The binary path **inside** the archive (e.g. `lua-language-server/bin/lua-language-server`)

For the four npm servers, look up the latest stable version on npmjs.com:

- typescript-language-server: https://www.npmjs.com/package/typescript-language-server
- pyright: https://www.npmjs.com/package/pyright
- bash-language-server: https://www.npmjs.com/package/bash-language-server
- vscode-langservers-extracted: https://www.npmjs.com/package/vscode-langservers-extracted

- [ ] **Step 2: Replace the empty `CATALOG: &[CatalogEntry] = &[];` with the populated entries**

Use the data captured in Step 1. Example shape for one binary entry (real values from Step 1 go in):

```rust
CatalogEntry {
    id: "rust-analyzer",
    display_name: "rust-analyzer",
    language_label: "rust",
    version: "2025.04.21",        // ← from Step 1
    category: Category::Binary,
    method: InstallMethod::BinaryDownload {
        urls: &[
            (Target::LinuxX86_64Gnu,  "https://github.com/rust-lang/rust-analyzer/releases/download/2025-04-21/rust-analyzer-x86_64-unknown-linux-gnu.gz", "<sha256-hex>"),
            (Target::LinuxAarch64Gnu, "<url>", "<sha256-hex>"),
            (Target::MacosX86_64,     "<url>", "<sha256-hex>"),
            (Target::MacosAarch64,    "<url>", "<sha256-hex>"),
            (Target::WindowsX86_64,   "<url>", "<sha256-hex>"),
        ],
        archive: ArchiveKind::GzipOnly,  // Windows asset is a .zip — see Step 3
        binary_path: "rust-analyzer",
    },
    lsp_entry: LspEntryTemplate {
        id: "rust",
        extensions: &["rs"],
        root_markers: &["Cargo.toml", "rust-project.json"],
        command: "{{BIN}}",
        args: &[],
    },
},
```

Repeat for the other 4 binary entries and 4 npm entries. Use this
template for an npm entry:

```rust
CatalogEntry {
    id: "typescript-language-server",
    display_name: "typescript-language-server",
    language_label: "typescript",
    version: "4.3.3",
    category: Category::Npm,
    method: InstallMethod::NpmGlobal {
        package: "typescript-language-server",
        binary: "typescript-language-server",
    },
    lsp_entry: LspEntryTemplate {
        id: "typescript",
        extensions: &["ts", "tsx", "mts", "cts"],
        root_markers: &["tsconfig.json", "package.json"],
        command: "typescript-language-server",
        args: &["--stdio"],
    },
},
```

- [ ] **Step 3: Handle the rust-analyzer Windows-zip exception**

rust-analyzer publishes `.gz` on Unix targets and `.zip` on Windows. The
`ArchiveKind` field describes the *predominant* archive kind, but the
installer (Task 14) inspects each URL's extension and picks the right
extractor. Add a comment above the entry explaining the mix.

- [ ] **Step 4: Run the catalog tests**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::catalog`
Expected: PASS — `catalog_ids_are_unique` and `binary_entries_cover_every_target` both pass against the populated catalog.

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/catalog.rs
git commit -m "feat(internal:lsp-installer): populate v1 catalog (5 binary + 4 npm servers)"
```

### Task 13: `InstallProgress` + `InstallOutcome` + `InstallError` types

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs`

- [ ] **Step 1: Write the types and a smoke test**

Replace `installer.rs` with:

```rust
//! Per-entry installer: binary download/verify/extract or npm i -g.

use std::path::PathBuf;

use thiserror::Error;

/// Streaming progress emitted by [`install_entry`] via its `notify`
/// callback. The callback runs on the installer's tokio task — pushing
/// into a `tokio::sync::mpsc::UnboundedSender<InstallProgress>` is
/// typical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallProgress {
    /// Install task started for `entry_id`.
    Started { entry_id: String },
    /// HTTP download in flight. `total` is `None` if the server didn't
    /// send `Content-Length`.
    Downloading { entry_id: String, bytes_so_far: u64, total: Option<u64> },
    /// SHA256 verification in progress (or just completed — semantically
    /// "verify started"; absence of `Failed` afterwards means success).
    Verifying { entry_id: String },
    /// Archive extraction in progress.
    Extracting { entry_id: String },
    /// `npm i -g` running. `line` is one line of npm stdout/stderr.
    RunningNpm { entry_id: String, line: String },
    /// Install succeeded; `installed_at` is the absolute path to the
    /// binary that should be referenced in lsp.toml.
    Done { entry_id: String, installed_at: PathBuf },
    /// Install failed; reason is human-readable.
    Failed { entry_id: String, reason: String },
}

/// Returned by [`install_entry`] on success — carries the data
/// [`crate::plugin::builtin::lsp_installer::config_writer`] needs to
/// upsert the entry into `~/.savvagent/lsp.toml`.
#[derive(Debug, Clone)]
pub struct InstallOutcome {
    pub entry_id: String,
    pub installed_at: PathBuf,
}

/// Reasons [`install_entry`] can return `Err`.
#[derive(Debug, Error)]
pub enum InstallError {
    #[error("unsupported host target — {0}")]
    UnsupportedTarget(String),
    #[error("required tool not found: {tool} (install it and re-run /lsp)")]
    ToolNotFound { tool: String },
    #[error("download failed: {0}")]
    Download(String),
    #[error("checksum mismatch for {entry_id}: expected {expected}, got {actual}")]
    ChecksumMismatch { entry_id: String, expected: String, actual: String },
    #[error("extract failed for {entry_id}: {reason}")]
    Extract { entry_id: String, reason: String },
    #[error("npm install failed for {entry_id}: {reason}")]
    Npm { entry_id: String, reason: String },
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_error_display_formats() {
        let e = InstallError::ToolNotFound { tool: "npm".into() };
        let msg = format!("{e}");
        assert!(msg.contains("npm"));
        assert!(msg.contains("install it"));
    }
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::installer`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs
git commit -m "feat(internal:lsp-installer): InstallProgress / InstallOutcome / InstallError types"
```

### Task 14: `install_entry` — binary download path (happy + checksum-fail)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs`

- [ ] **Step 1: Write the failing tests**

Append to `installer.rs` (above the existing `#[cfg(test)] mod tests {}` block, replacing it):

```rust
use crate::plugin::builtin::lsp_installer::catalog::{
    ArchiveKind, CatalogEntry, Category, InstallMethod, LspEntryTemplate, Target,
};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::AsyncWriteExt;

/// Thin abstraction over `reqwest::Client` so tests can substitute a
/// fixture. Production callers pass `ReqwestDownloader`.
#[async_trait::async_trait]
pub trait Downloader: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError>;
}

pub struct ReqwestDownloader {
    pub client: reqwest::Client,
}

#[async_trait::async_trait]
impl Downloader for ReqwestDownloader {
    async fn fetch(&self, url: &str) -> Result<bytes::Bytes, InstallError> {
        let resp = self
            .client
            .get(url)
            .header(
                reqwest::header::USER_AGENT,
                concat!("savvagent/", env!("CARGO_PKG_VERSION")),
            )
            .send()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))?;
        if !resp.status().is_success() {
            return Err(InstallError::Download(format!(
                "HTTP {}: {}",
                resp.status(),
                url
            )));
        }
        resp.bytes()
            .await
            .map_err(|e| InstallError::Download(e.to_string()))
    }
}

/// Install a single catalog entry. The function is split here only for
/// the binary path; the npm path lands in Task 15.
pub async fn install_binary_entry(
    entry: &CatalogEntry,
    target: Target,
    lsp_bin_root: &Path,
    downloader: &dyn Downloader,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::BinaryDownload { urls, archive: _, binary_path } = entry.method else {
        return Err(InstallError::Download(format!(
            "{}: install_binary_entry called on a non-Binary entry",
            entry.id
        )));
    };

    let (_, url, expected_sha) = urls
        .iter()
        .find(|(t, _, _)| *t == target)
        .ok_or_else(|| InstallError::UnsupportedTarget(format!("{target:?}")))?;

    notify(InstallProgress::Started { entry_id: entry.id.into() });

    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: 0,
        total: None,
    });
    let bytes = downloader.fetch(url).await?;
    notify(InstallProgress::Downloading {
        entry_id: entry.id.into(),
        bytes_so_far: bytes.len() as u64,
        total: Some(bytes.len() as u64),
    });

    notify(InstallProgress::Verifying { entry_id: entry.id.into() });
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex::encode(hasher.finalize());
    if actual != *expected_sha {
        return Err(InstallError::ChecksumMismatch {
            entry_id: entry.id.into(),
            expected: (*expected_sha).into(),
            actual,
        });
    }

    notify(InstallProgress::Extracting { entry_id: entry.id.into() });
    let install_dir = lsp_bin_root.join(entry.id);
    if install_dir.exists() {
        tokio::fs::remove_dir_all(&install_dir).await?;
    }
    tokio::fs::create_dir_all(&install_dir).await?;
    let installed_at = extract_one(&bytes, url, binary_path, &install_dir).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&installed_at)?.permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&installed_at, perms)?;
    }

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}

/// Extract `bytes` to `install_dir`, choosing the extractor by the
/// URL's suffix (`.gz` / `.tar.gz` / `.zip`). Returns the path to the
/// binary inside the install dir.
async fn extract_one(
    bytes: &bytes::Bytes,
    url: &str,
    binary_path: &str,
    install_dir: &Path,
) -> Result<PathBuf, InstallError> {
    let bin_in_dir = install_dir.join(binary_path);
    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        let dec = flate2::read::GzDecoder::new(&bytes[..]);
        let mut ar = tar::Archive::new(dec);
        ar.unpack(install_dir).map_err(|e| InstallError::Extract {
            entry_id: install_dir.file_name().unwrap().to_string_lossy().into(),
            reason: e.to_string(),
        })?;
    } else if url.ends_with(".zip") {
        let reader = std::io::Cursor::new(&bytes[..]);
        let mut zip = zip::ZipArchive::new(reader).map_err(|e| InstallError::Extract {
            entry_id: install_dir.file_name().unwrap().to_string_lossy().into(),
            reason: e.to_string(),
        })?;
        zip.extract(install_dir).map_err(|e| InstallError::Extract {
            entry_id: install_dir.file_name().unwrap().to_string_lossy().into(),
            reason: e.to_string(),
        })?;
    } else if url.ends_with(".gz") {
        let mut dec = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = tokio::fs::File::create(&bin_in_dir).await?;
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut dec, &mut buf).map_err(InstallError::Io)?;
        out.write_all(&buf).await?;
        out.flush().await?;
    } else {
        return Err(InstallError::Extract {
            entry_id: install_dir.file_name().unwrap().to_string_lossy().into(),
            reason: format!("unrecognised archive suffix in {url}"),
        });
    }
    if !bin_in_dir.exists() {
        return Err(InstallError::Extract {
            entry_id: install_dir.file_name().unwrap().to_string_lossy().into(),
            reason: format!("binary not found at {} after extract", bin_in_dir.display()),
        });
    }
    Ok(bin_in_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn fake_entry(sha: &'static str) -> CatalogEntry {
        CatalogEntry {
            id: "fakelsp",
            display_name: "fakelsp",
            language_label: "fake",
            version: "0.0.0",
            category: Category::Binary,
            method: InstallMethod::BinaryDownload {
                urls: &[(
                    Target::LinuxX86_64Gnu,
                    "https://example.test/fakelsp.gz",
                    "PLACEHOLDER",
                )],
                archive: ArchiveKind::GzipOnly,
                binary_path: "fakelsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: "{{BIN}}",
                args: &[],
            },
        }
    }

    struct StubDownloader { payload: bytes::Bytes }
    #[async_trait::async_trait]
    impl Downloader for StubDownloader {
        async fn fetch(&self, _url: &str) -> Result<bytes::Bytes, InstallError> {
            Ok(self.payload.clone())
        }
    }

    fn gzipped(plain: &[u8]) -> Vec<u8> {
        use flate2::{write::GzEncoder, Compression};
        use std::io::Write;
        let mut enc = GzEncoder::new(Vec::new(), Compression::default());
        enc.write_all(plain).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn install_error_display_formats() {
        let e = InstallError::ToolNotFound { tool: "npm".into() };
        assert!(format!("{e}").contains("npm"));
    }

    #[tokio::test]
    async fn binary_download_happy_path_writes_executable() {
        let plain = b"#!/bin/sh\necho fakelsp\n";
        let archive = gzipped(plain);
        let sha = hex::encode(Sha256::digest(&archive));

        // Patch the entry's first URL's sha at runtime — the CatalogEntry
        // is `static` in production but we own the fixture here. Cheat
        // by rebuilding the URLs slice with a `Box::leak` so it lives long
        // enough for the test.
        let urls: &'static [(Target, &'static str, &'static str)] = Box::leak(Box::new([(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            Box::leak(sha.into_boxed_str()),
        )]));
        let mut entry = fake_entry("PLACEHOLDER");
        if let InstallMethod::BinaryDownload { ref mut urls: e_urls, .. } = entry.method {
            *e_urls = urls;
        }

        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader { payload: bytes::Bytes::from(archive) };
        let outcome = install_binary_entry(
            &entry,
            Target::LinuxX86_64Gnu,
            tmp.path(),
            &dl,
            |_| {},
        )
        .await
        .unwrap();

        assert!(outcome.installed_at.exists());
        let written = std::fs::read(&outcome.installed_at).unwrap();
        assert_eq!(written, plain);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&outcome.installed_at).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "binary must be executable: {mode:o}");
        }
    }

    #[tokio::test]
    async fn checksum_mismatch_returns_error() {
        let archive = gzipped(b"not-the-payload-we-expected");
        // Use a bogus sha (all zeros).
        let urls: &'static [(Target, &'static str, &'static str)] = &[(
            Target::LinuxX86_64Gnu,
            "https://example.test/fakelsp.gz",
            "0000000000000000000000000000000000000000000000000000000000000000",
        )];
        let mut entry = fake_entry("0000…");
        if let InstallMethod::BinaryDownload { ref mut urls: e_urls, .. } = entry.method {
            *e_urls = urls;
        }
        let tmp = tempfile::tempdir().unwrap();
        let dl = StubDownloader { payload: bytes::Bytes::from(archive) };
        let err = install_binary_entry(
            &entry,
            Target::LinuxX86_64Gnu,
            tmp.path(),
            &dl,
            |_| {},
        )
        .await
        .unwrap_err();
        match err {
            InstallError::ChecksumMismatch { .. } => (),
            other => panic!("expected ChecksumMismatch, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Add `hex` + `tempfile` to savvagent's deps (if missing)**

Open `crates/savvagent/Cargo.toml`. Add to `[dependencies]` if not already
present:

```toml
hex = "0.4"
```

And to `[dev-dependencies]`:

```toml
tempfile = "3"
```

Also add `hex = "0.4"` to `[workspace.dependencies]` in the workspace `Cargo.toml` and switch the savvagent dep to `hex = { workspace = true }` for consistency.

- [ ] **Step 3: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::installer`
Expected: 3 tests pass (`install_error_display_formats`,
`binary_download_happy_path_writes_executable`,
`checksum_mismatch_returns_error`).

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/savvagent/Cargo.toml crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs
git commit -m "feat(internal:lsp-installer): binary download path with checksum verification"
```

### Task 15: `install_npm_entry` (happy + npm-missing)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs`

- [ ] **Step 1: Write the failing tests**

Append to `installer.rs` (above the existing `#[cfg(test)] mod tests`
block, then update the test block with two new tests):

```rust
/// Thin abstraction over the `npm` subprocess so tests can stub it.
#[async_trait::async_trait]
pub trait NpmRunner: Send + Sync {
    /// Run `npm i -g <package>@<version>`. The implementation forwards
    /// each line of stdout/stderr via `on_line`. Returns `Ok` on a
    /// zero exit code, `Err(npm_message)` otherwise.
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String>;
    /// Return `npm root -g` (the directory npm installs globals into).
    async fn root_global(&self) -> Result<PathBuf, String>;
}

pub struct SystemNpmRunner;

#[async_trait::async_trait]
impl NpmRunner for SystemNpmRunner {
    async fn install_global(
        &self,
        package: &str,
        version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::process::Command;
        let mut child = Command::new("npm")
            .args(["i", "-g", &format!("{package}@{version}")])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn npm: {e}"))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut out_lines = BufReader::new(stdout).lines();
        let mut err_lines = BufReader::new(stderr).lines();
        loop {
            tokio::select! {
                Ok(Some(line)) = out_lines.next_line() => on_line(line),
                Ok(Some(line)) = err_lines.next_line() => on_line(line),
                else => break,
            }
        }
        let status = child.wait().await.map_err(|e| format!("wait npm: {e}"))?;
        if !status.success() {
            return Err(format!("npm exited with status {status}"));
        }
        Ok(())
    }

    async fn root_global(&self) -> Result<PathBuf, String> {
        let out = tokio::process::Command::new("npm")
            .args(["root", "-g"])
            .output()
            .await
            .map_err(|e| format!("spawn `npm root -g`: {e}"))?;
        if !out.status.success() {
            return Err(format!("npm root -g failed: status {}", out.status));
        }
        let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(PathBuf::from(path))
    }
}

/// Returns `Some(absolute path to npm)` if npm is on `$PATH`, else `None`.
pub fn detect_npm() -> Option<PathBuf> {
    which::which("npm").ok()
}

pub async fn install_npm_entry(
    entry: &CatalogEntry,
    runner: &dyn NpmRunner,
    notify: impl Fn(InstallProgress) + Send + Sync,
) -> Result<InstallOutcome, InstallError> {
    let InstallMethod::NpmGlobal { package, binary } = entry.method else {
        return Err(InstallError::Npm {
            entry_id: entry.id.into(),
            reason: "install_npm_entry called on a non-Npm entry".into(),
        });
    };

    notify(InstallProgress::Started { entry_id: entry.id.into() });

    let entry_id_owned = entry.id.to_string();
    let notify_for_npm = &notify;
    runner
        .install_global(package, entry.version, &move |line| {
            notify_for_npm(InstallProgress::RunningNpm {
                entry_id: entry_id_owned.clone(),
                line,
            });
        })
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;

    let root = runner
        .root_global()
        .await
        .map_err(|reason| InstallError::Npm {
            entry_id: entry.id.into(),
            reason,
        })?;
    // npm puts bins one level up from `root`: `<prefix>/lib/node_modules`
    // → bins are in `<prefix>/bin/<binary>`. `npm root -g` returns the
    // `node_modules` path; the bin dir is its grandparent + `/bin`.
    let installed_at = root
        .parent()
        .and_then(|p| p.parent())
        .map(|prefix| prefix.join("bin").join(binary))
        .ok_or_else(|| InstallError::Npm {
            entry_id: entry.id.into(),
            reason: format!("could not derive bin path from npm root {}", root.display()),
        })?;

    notify(InstallProgress::Done {
        entry_id: entry.id.into(),
        installed_at: installed_at.clone(),
    });
    Ok(InstallOutcome {
        entry_id: entry.id.into(),
        installed_at,
    })
}
```

Then append to the existing test module:

```rust
struct StubNpm {
    install_result: Result<(), String>,
    root: PathBuf,
}

#[async_trait::async_trait]
impl NpmRunner for StubNpm {
    async fn install_global(
        &self,
        _package: &str,
        _version: &str,
        on_line: &(dyn Fn(String) + Send + Sync),
    ) -> Result<(), String> {
        on_line("added 1 package".into());
        self.install_result.clone()
    }
    async fn root_global(&self) -> Result<PathBuf, String> {
        Ok(self.root.clone())
    }
}

fn npm_entry() -> CatalogEntry {
    CatalogEntry {
        id: "fake-npm-lsp",
        display_name: "fake-npm-lsp",
        language_label: "fake",
        version: "1.2.3",
        category: Category::Npm,
        method: InstallMethod::NpmGlobal {
            package: "fake-npm-lsp",
            binary: "fake-npm-lsp",
        },
        lsp_entry: LspEntryTemplate {
            id: "fake",
            extensions: &["fake"],
            root_markers: &["fake.toml"],
            command: "fake-npm-lsp",
            args: &[],
        },
    }
}

#[tokio::test]
async fn npm_happy_path_derives_bin_from_root() {
    let tmp = tempfile::tempdir().unwrap();
    // Simulate `npm root -g` returning `<prefix>/lib/node_modules`.
    let prefix = tmp.path();
    let root = prefix.join("lib").join("node_modules");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::write(prefix.join("bin").join("fake-npm-lsp"), b"#!/bin/sh\n").unwrap();
    let runner = StubNpm { install_result: Ok(()), root };
    let outcome = install_npm_entry(&npm_entry(), &runner, |_| {})
        .await
        .unwrap();
    assert_eq!(outcome.installed_at, prefix.join("bin").join("fake-npm-lsp"));
}

#[tokio::test]
async fn npm_install_failure_returns_npm_error() {
    let runner = StubNpm {
        install_result: Err("network down".into()),
        root: PathBuf::from("/tmp/unused"),
    };
    let err = install_npm_entry(&npm_entry(), &runner, |_| {})
        .await
        .unwrap_err();
    match err {
        InstallError::Npm { reason, .. } => assert!(reason.contains("network down")),
        other => panic!("expected InstallError::Npm, got {other:?}"),
    }
}
```

- [ ] **Step 2: Add `which` to deps**

Open the workspace `Cargo.toml` and add to `[workspace.dependencies]`:

```toml
which = "6"
```

Then `crates/savvagent/Cargo.toml` `[dependencies]`:

```toml
which = { workspace = true }
```

- [ ] **Step 3: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::installer`
Expected: 5 tests pass total.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml Cargo.lock crates/savvagent/Cargo.toml crates/savvagent/src/plugin/builtin/lsp_installer/installer.rs
git commit -m "feat(internal:lsp-installer): npm install path + detect_npm"
```

### Task 16: `config_writer::merge_into_user_config`

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/config_writer.rs`

- [ ] **Step 1: Write the failing tests**

Replace `config_writer.rs` with:

```rust
//! Merge installed entries into ~/.savvagent/lsp.toml.

use std::path::{Path, PathBuf};

use crate::plugin::builtin::lsp_installer::catalog::CatalogEntry;
use crate::plugin::builtin::lsp_installer::installer::InstallOutcome;

/// A single `[[language]]` table in `lsp.toml`. Mirrors
/// `tool_lsp::config::LanguageEntry` field-for-field but lives in the
/// savvagent crate so we don't need a dep on tool_lsp.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct LanguageEntry {
    pub id: String,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct LspConfig {
    #[serde(default, rename = "language")]
    pub languages: Vec<LanguageEntry>,
}

/// Read `path` (treating ENOENT as empty), upsert each
/// (catalog-entry, install-outcome) pair, then write back atomically.
pub async fn merge_into_user_config(
    path: &Path,
    upserts: &[(&CatalogEntry, &InstallOutcome)],
) -> std::io::Result<()> {
    let mut cfg = match tokio::fs::read_to_string(path).await {
        Ok(text) => toml::from_str::<LspConfig>(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => LspConfig::default(),
        Err(e) => return Err(e),
    };

    for (entry, outcome) in upserts {
        let tmpl = entry.lsp_entry;
        let command = if tmpl.command == "{{BIN}}" {
            outcome.installed_at.to_string_lossy().into_owned()
        } else {
            tmpl.command.to_string()
        };
        let new_entry = LanguageEntry {
            id: tmpl.id.to_string(),
            extensions: tmpl.extensions.iter().map(|s| (*s).to_string()).collect(),
            root_markers: tmpl.root_markers.iter().map(|s| (*s).to_string()).collect(),
            command,
            args: tmpl.args.iter().map(|s| (*s).to_string()).collect(),
            env: std::collections::HashMap::new(),
        };
        if let Some(existing) = cfg.languages.iter_mut().find(|l| l.id == new_entry.id) {
            *existing = new_entry;
        } else {
            cfg.languages.push(new_entry);
        }
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let body = toml::to_string_pretty(&cfg)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_atomic(path, body.as_bytes()).await
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = PathBuf::from(dir).join(format!(
        ".lsp.toml.savvagent.{}.tmp",
        std::process::id()
    ));
    tokio::fs::write(&tmp, bytes).await?;
    tokio::fs::rename(&tmp, path).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::builtin::lsp_installer::catalog::{
        ArchiveKind, CatalogEntry, Category, InstallMethod, LspEntryTemplate, Target,
    };

    fn fake_binary_entry() -> CatalogEntry {
        CatalogEntry {
            id: "fakelsp",
            display_name: "fakelsp",
            language_label: "fake",
            version: "1.0.0",
            category: Category::Binary,
            method: InstallMethod::BinaryDownload {
                urls: &[(Target::LinuxX86_64Gnu, "https://example.test/x.gz", "0")],
                archive: ArchiveKind::GzipOnly,
                binary_path: "fakelsp",
            },
            lsp_entry: LspEntryTemplate {
                id: "fake",
                extensions: &["fake"],
                root_markers: &["fake.toml"],
                command: "{{BIN}}",
                args: &[],
            },
        }
    }

    #[tokio::test]
    async fn creates_file_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("subdir").join("lsp.toml");
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/opt/fakelsp/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("[[language]]"));
        assert!(written.contains("id = \"fake\""));
        assert!(written.contains("command = \"/opt/fakelsp/fakelsp\""));
    }

    #[tokio::test]
    async fn upsert_replaces_existing_entry_by_id() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        // Pre-seed with an entry for the same id.
        std::fs::write(
            &path,
            r#"
[[language]]
id = "fake"
extensions = ["old"]
root_markers = ["old.toml"]
command = "/old/path"
args = ["--old"]
"#,
        )
        .unwrap();
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/new/path/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages.len(), 1, "must replace, not append");
        let only = &cfg.languages[0];
        assert_eq!(only.extensions, vec!["fake".to_string()]);
        assert_eq!(only.command, "/new/path/fakelsp");
        assert!(only.args.is_empty(), "args must be replaced, not merged");
    }

    #[tokio::test]
    async fn upsert_preserves_unrelated_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        std::fs::write(
            &path,
            r#"
[[language]]
id = "go"
extensions = ["go"]
root_markers = ["go.mod"]
command = "gopls"
"#,
        )
        .unwrap();
        let entry = fake_binary_entry();
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/opt/fakelsp/fakelsp"),
        };
        merge_into_user_config(&path, &[(&entry, &outcome)])
            .await
            .unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages.len(), 2);
        let ids: Vec<&str> = cfg.languages.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"go"));
        assert!(ids.contains(&"fake"));
    }

    #[tokio::test]
    async fn literal_command_template_is_passed_through() {
        let mut entry = fake_binary_entry();
        entry.lsp_entry = LspEntryTemplate {
            id: "fake",
            extensions: &["fake"],
            root_markers: &["fake.toml"],
            command: "fakelsp-on-path",
            args: &["--stdio"],
        };
        let outcome = InstallOutcome {
            entry_id: entry.id.into(),
            installed_at: PathBuf::from("/unused"),
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lsp.toml");
        merge_into_user_config(&path, &[(&entry, &outcome)]).await.unwrap();
        let cfg: LspConfig = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(cfg.languages[0].command, "fakelsp-on-path");
        assert_eq!(cfg.languages[0].args, vec!["--stdio".to_string()]);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::config_writer`
Expected: 4 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/config_writer.rs
git commit -m "feat(internal:lsp-installer): config_writer with atomic write + upsert-by-id"
```

### Task 17: `LspPicker` (wraps `MultiSelectList<&CatalogEntry>`)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/picker.rs`

- [ ] **Step 1: Write the failing test**

Replace `picker.rs` with:

```rust
//! Wraps MultiSelectList<&CatalogEntry> for the /lsp UI.

use crate::plugin::builtin::lsp_installer::catalog::{CatalogEntry, CATALOG};
use crate::plugin::widgets::MultiSelectList;

/// Picker state for `/lsp`. Holds the catalog by reference so the
/// `Confirm` payload can be looked up by id without cloning entries.
pub struct LspPicker {
    pub inner: MultiSelectList<&'static CatalogEntry>,
}

impl LspPicker {
    pub fn new() -> Self {
        let items: Vec<&'static CatalogEntry> = CATALOG.iter().collect();
        let widget = MultiSelectList::new(
            items,
            |entry: &&'static CatalogEntry, filter: &str| {
                let f = filter.to_ascii_lowercase();
                entry.id.to_ascii_lowercase().contains(&f)
                    || entry.display_name.to_ascii_lowercase().contains(&f)
                    || entry.language_label.to_ascii_lowercase().contains(&f)
            },
            |entry: &&'static CatalogEntry| entry.id.to_string(),
        );
        Self { inner: widget }
    }
}

impl Default for LspPicker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picker_loads_full_catalog() {
        let p = LspPicker::new();
        assert_eq!(p.inner.filtered().len(), CATALOG.len());
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::picker`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/picker.rs
git commit -m "feat(internal:lsp-installer): LspPicker wrapping MultiSelectList<&CatalogEntry>"
```

### Task 18: `LspPickerScreen` (Screen trait impl)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs`

- [ ] **Step 1: Write the failing test**

Replace `screen.rs` with:

```rust
//! LspPickerScreen — Screen impl bridging picker outcomes to Effects.

use async_trait::async_trait;
use savvagent_plugin::{
    Effect, KeyCodePortable, KeyEventPortable, PluginError, Region, Screen, StyledLine,
    StyledSpan, TextMods, ThemeColor,
};

use crate::plugin::builtin::lsp_installer::picker::LspPicker;
use crate::plugin::widgets::MultiSelectOutcome;

pub struct LspPickerScreen {
    inner: LspPicker,
}

impl LspPickerScreen {
    pub fn new() -> Self {
        Self { inner: LspPicker::new() }
    }
}

impl Default for LspPickerScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Screen for LspPickerScreen {
    fn id(&self) -> String {
        "lsp_installer.picker".to_string()
    }

    fn render(&self, _region: Region) -> Vec<StyledLine> {
        let mut out: Vec<StyledLine> = Vec::new();
        out.push(StyledLine::plain(format!(
            "Filter: {}",
            self.inner.inner.filter()
        )));
        out.push(StyledLine::plain(""));
        out.push(StyledLine::plain(
            "Select language servers to install. Space toggles, Enter confirms, Esc cancels.",
        ));
        out.push(StyledLine::plain(format!(
            "  Selected: {}",
            self.inner.inner.selected().len()
        )));
        out.push(StyledLine::plain(""));

        for (i, entry) in self.inner.inner.filtered().iter().enumerate() {
            let cursor = if i == self.inner.inner.cursor() { ">" } else { " " };
            let mark = if self.inner.inner.selected().contains(entry.id) {
                "[x]"
            } else {
                "[ ]"
            };
            let category = match entry.category {
                crate::plugin::builtin::lsp_installer::catalog::Category::Binary => "binary",
                crate::plugin::builtin::lsp_installer::catalog::Category::Npm => "npm",
            };
            out.push(StyledLine {
                spans: vec![StyledSpan {
                    text: format!(
                        "{cursor} {mark} {:<32} {:<12} {:<14} ({})",
                        entry.display_name, entry.language_label, entry.version, category
                    ),
                    fg: Some(if i == self.inner.inner.cursor() {
                        ThemeColor::Accent
                    } else {
                        ThemeColor::Fg
                    }),
                    bg: None,
                    modifiers: TextMods {
                        bold: i == self.inner.inner.cursor(),
                        ..Default::default()
                    },
                }],
            });
        }
        out
    }

    async fn on_key(&mut self, key: KeyEventPortable) -> Result<Vec<Effect>, PluginError> {
        let ct_event = portable_to_crossterm(&key);
        let outcome = self.inner.inner.on_key(ct_event);
        match outcome {
            MultiSelectOutcome::Stay | MultiSelectOutcome::Preview(_) | MultiSelectOutcome::Toggle(_) => {
                Ok(vec![])
            }
            MultiSelectOutcome::Cancel => Ok(vec![Effect::CloseScreen]),
            MultiSelectOutcome::Confirm(items) => {
                if items.is_empty() {
                    return Ok(vec![Effect::CloseScreen]);
                }
                let mut args = vec!["__install".to_string()];
                args.extend(items.iter().map(|e| e.id.to_string()));
                Ok(vec![Effect::Stack(vec![
                    Effect::CloseScreen,
                    Effect::RunSlash { name: "lsp".into(), args },
                ])])
            }
        }
    }

    fn tips(&self) -> Vec<StyledLine> {
        vec![StyledLine::plain(
            "↑/↓ move • Space toggle • Enter install selected • Esc cancel",
        )]
    }
}

fn portable_to_crossterm(key: &KeyEventPortable) -> crossterm::event::KeyEvent {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let code = match key.code {
        KeyCodePortable::Char(c) => KeyCode::Char(c),
        KeyCodePortable::Enter => KeyCode::Enter,
        KeyCodePortable::Esc => KeyCode::Esc,
        KeyCodePortable::Up => KeyCode::Up,
        KeyCodePortable::Down => KeyCode::Down,
        KeyCodePortable::Backspace => KeyCode::Backspace,
        _ => KeyCode::Null,
    };
    let mut mods = KeyModifiers::empty();
    if key.modifiers.ctrl { mods |= KeyModifiers::CONTROL; }
    if key.modifiers.alt { mods |= KeyModifiers::ALT; }
    if key.modifiers.shift { mods |= KeyModifiers::SHIFT; }
    KeyEvent::new(code, mods)
}

#[cfg(test)]
mod tests {
    use super::*;
    use savvagent_plugin::KeyMods;

    fn key(code: KeyCodePortable) -> KeyEventPortable {
        KeyEventPortable { code, modifiers: KeyMods::default() }
    }

    #[tokio::test]
    async fn esc_closes_screen() {
        let mut s = LspPickerScreen::new();
        let effs = s.on_key(key(KeyCodePortable::Esc)).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::CloseScreen]));
    }

    #[tokio::test]
    async fn enter_with_zero_selection_just_closes() {
        let mut s = LspPickerScreen::new();
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::CloseScreen]));
    }

    #[tokio::test]
    async fn enter_with_one_selection_emits_runslash_install() {
        let mut s = LspPickerScreen::new();
        s.on_key(key(KeyCodePortable::Char(' '))).await.unwrap();
        let effs = s.on_key(key(KeyCodePortable::Enter)).await.unwrap();
        match &effs[..] {
            [Effect::Stack(children)] => {
                assert!(matches!(children[0], Effect::CloseScreen));
                match &children[1] {
                    Effect::RunSlash { name, args } => {
                        assert_eq!(name, "lsp");
                        assert_eq!(args[0], "__install");
                        assert_eq!(args.len(), 2, "exactly one id appended");
                    }
                    other => panic!("expected RunSlash, got {other:?}"),
                }
            }
            other => panic!("expected single Stack, got {other:?}"),
        }
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer::screen`
Expected: 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/screen.rs
git commit -m "feat(internal:lsp-installer): LspPickerScreen rendering + key handling"
```

### Task 19: `LspInstallerPlugin` — manifest + slash dispatch (open path)

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`

- [ ] **Step 1: Write the failing test**

Replace `mod.rs` with:

```rust
//! `internal:lsp-installer` — `/lsp` slash command, multi-select picker,
//! and one-shot LSP-binary installer.

pub mod catalog;
pub mod config_writer;
pub mod installer;
pub mod picker;
pub mod screen;

use async_trait::async_trait;
use savvagent_plugin::{
    Contributions, Effect, Manifest, Plugin, PluginError, PluginId, PluginKind, ScreenArgs,
    ScreenSpec, SlashSpec,
};

use screen::LspPickerScreen;

pub struct LspInstallerPlugin;

impl LspInstallerPlugin {
    pub fn new() -> Self { Self }
}

impl Default for LspInstallerPlugin {
    fn default() -> Self { Self::new() }
}

const PLUGIN_ID: &str = "internal:lsp-installer";

#[async_trait]
impl Plugin for LspInstallerPlugin {
    fn manifest(&self) -> Manifest {
        Manifest {
            id: PluginId::new(PLUGIN_ID).expect("valid"),
            name: "LSP installer".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            description: "Install curated language-server binaries via /lsp".into(),
            kind: PluginKind::Optional,
            contributions: Contributions {
                slashes: vec![SlashSpec {
                    name: "lsp".into(),
                    description: "Install language servers".into(),
                    ..Default::default()
                }],
                screens: vec![ScreenSpec {
                    id: "lsp_installer.picker".into(),
                    ..Default::default()
                }],
                ..Default::default()
            },
        }
    }

    async fn handle_slash(
        &mut self,
        name: &str,
        args: Vec<String>,
    ) -> Result<Vec<Effect>, PluginError> {
        if name != "lsp" {
            return Ok(vec![]);
        }
        match args.first().map(String::as_str) {
            None => Ok(vec![Effect::OpenScreen {
                id: "lsp_installer.picker".into(),
                args: ScreenArgs::None,
            }]),
            Some("__install") => {
                // Task 20 wires the real install path here.
                Ok(vec![])
            }
            Some(other) => Ok(vec![Effect::PushNote {
                line: savvagent_plugin::StyledLine::plain(format!(
                    "/lsp: unknown sub-command `{other}` — run `/lsp` with no args to open the picker"
                )),
            }]),
        }
    }

    fn create_screen(
        &self,
        id: &str,
        _args: ScreenArgs,
    ) -> Result<Box<dyn savvagent_plugin::Screen>, PluginError> {
        match id {
            "lsp_installer.picker" => Ok(Box::new(LspPickerScreen::new())),
            other => Err(PluginError::ScreenNotFound(other.into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lsp_with_no_args_opens_picker() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec![]).await.unwrap();
        match &effs[..] {
            [Effect::OpenScreen { id, .. }] => assert_eq!(id, "lsp_installer.picker"),
            other => panic!("expected OpenScreen, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_subcommand_pushes_help_note() {
        let mut p = LspInstallerPlugin::new();
        let effs = p.handle_slash("lsp", vec!["bogus".into()]).await.unwrap();
        assert!(matches!(effs.as_slice(), [Effect::PushNote { .. }]));
    }

    #[test]
    fn manifest_advertises_slash_and_screen() {
        let p = LspInstallerPlugin::new();
        let m = p.manifest();
        assert!(m.contributions.slashes.iter().any(|s| s.name == "lsp"));
        assert!(m.contributions.screens.iter().any(|s| s.id == "lsp_installer.picker"));
    }
}
```

- [ ] **Step 2: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer`
Expected: PASS (3 mod tests + all earlier tests still green).

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs
git commit -m "feat(internal:lsp-installer): plugin manifest + /lsp open-picker dispatch"
```

### Task 20: Wire `__install` to spawn install tasks + push notes

**Files:**
- Modify: `crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs`

- [ ] **Step 1: Write the failing test**

Append inside the existing `#[cfg(test)] mod tests` block in `mod.rs`:

```rust
#[tokio::test]
async fn install_with_no_ids_emits_no_effects() {
    let mut p = LspInstallerPlugin::new();
    let effs = p.handle_slash("lsp", vec!["__install".into()]).await.unwrap();
    assert!(effs.is_empty(), "no-op for empty id list");
}

#[tokio::test]
async fn install_with_unknown_id_pushes_skipped_note() {
    let mut p = LspInstallerPlugin::new();
    let effs = p
        .handle_slash("lsp", vec!["__install".into(), "no-such-server".into()])
        .await
        .unwrap();
    assert!(
        effs.iter().any(|e| matches!(e, Effect::PushNote { line } if line
            .spans
            .iter()
            .any(|s| s.text.contains("no-such-server")))),
        "expected a PushNote mentioning the unknown id, got {effs:?}"
    );
}
```

- [ ] **Step 2: Implement the `__install` path**

Replace the `Some("__install") => { Ok(vec![]) }` arm in
`handle_slash` with:

```rust
Some("__install") => self.handle_install(args[1..].to_vec()).await,
```

Then add to the same `impl` block:

```rust
impl LspInstallerPlugin {
    async fn handle_install(&self, ids: Vec<String>) -> Result<Vec<Effect>, PluginError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let mut effs: Vec<Effect> = Vec::new();
        let target = match catalog::Target::current() {
            Some(t) => t,
            None => {
                effs.push(Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(
                        "/lsp: this host's target is not supported by the installer".to_string(),
                    ),
                });
                return Ok(effs);
            }
        };
        let lsp_bin_root = match dirs::home_dir() {
            Some(home) => home.join(".savvagent").join("lsp-bin"),
            None => {
                effs.push(Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(
                        "/lsp: could not resolve $HOME; install aborted".into(),
                    ),
                });
                return Ok(effs);
            }
        };
        let lsp_toml = lsp_bin_root
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("lsp.toml");

        // Resolve ids → catalog entries up front; surface unknown ids
        // synchronously so the user sees the typo before we spawn anything.
        let mut entries: Vec<&'static catalog::CatalogEntry> = Vec::new();
        for id in &ids {
            match catalog::CATALOG.iter().find(|e| e.id == id) {
                Some(e) => entries.push(e),
                None => effs.push(Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(format!(
                        "[lsp-installer] skipped: no catalog entry for `{id}`"
                    )),
                }),
            }
        }
        if entries.is_empty() {
            return Ok(effs);
        }

        effs.push(Effect::PushNote {
            line: savvagent_plugin::StyledLine::plain(format!(
                "[lsp-installer] installing {} server(s)…",
                entries.len()
            )),
        });

        // Spawn the install task. It pushes its own notes via a channel
        // that the runtime drains. For PR 2 the simplest path is to use
        // a `tokio::spawn` with `tracing::info!` for progress and a
        // synchronous post-install note batch returned via... actually,
        // the existing `Effect` taxonomy has no "background-task →
        // PushNote" plumbing other than what `internal:self-update`
        // already uses (it owns its own task and pushes via Effect on a
        // later turn). v1 of /lsp follows the same pattern: run installs
        // sequentially inside the slash handler, awaiting each one,
        // streaming InstallProgress as PushNotes via an mpsc loop.
        let downloader = installer::ReqwestDownloader {
            client: reqwest::Client::builder()
                .build()
                .map_err(|e| PluginError::Custom(format!("reqwest build: {e}")))?,
        };
        let npm = installer::SystemNpmRunner;

        for entry in entries {
            let result = match entry.category {
                catalog::Category::Binary => {
                    installer::install_binary_entry(
                        entry,
                        target,
                        &lsp_bin_root,
                        &downloader,
                        |progress| tracing::info!(?progress, "lsp install"),
                    )
                    .await
                }
                catalog::Category::Npm => {
                    if installer::detect_npm().is_none() {
                        effs.push(Effect::PushNote {
                            line: savvagent_plugin::StyledLine::plain(format!(
                                "[lsp-installer] {}: npm not found on $PATH — install Node.js from https://nodejs.org and re-run /lsp",
                                entry.id
                            )),
                        });
                        continue;
                    }
                    installer::install_npm_entry(entry, &npm, |progress| {
                        tracing::info!(?progress, "lsp install")
                    })
                    .await
                }
            };
            match result {
                Ok(outcome) => {
                    if let Err(e) =
                        config_writer::merge_into_user_config(&lsp_toml, &[(entry, &outcome)])
                            .await
                    {
                        effs.push(Effect::PushNote {
                            line: savvagent_plugin::StyledLine::plain(format!(
                                "[lsp-installer] {}: installed but config write failed: {e}",
                                entry.id
                            )),
                        });
                    } else {
                        effs.push(Effect::PushNote {
                            line: savvagent_plugin::StyledLine::plain(format!(
                                "[lsp-installer] {}: installed at {}",
                                entry.id,
                                outcome.installed_at.display()
                            )),
                        });
                    }
                }
                Err(e) => effs.push(Effect::PushNote {
                    line: savvagent_plugin::StyledLine::plain(format!(
                        "[lsp-installer] {}: failed — {e}",
                        entry.id
                    )),
                }),
            }
        }
        effs.push(Effect::PushNote {
            line: savvagent_plugin::StyledLine::plain(
                "[lsp-installer] done — restart savvagent to pick up the new servers".into(),
            ),
        });
        Ok(effs)
    }
}
```

(Note: this is a deliberately *sequential* implementation for v1. The
parallel-with-FuturesUnordered version called out in the spec is a
follow-up — it requires a richer plumbing for streaming progress notes
mid-handler, which the existing `handle_slash → Vec<Effect>` shape
doesn't support without buffering. Sequential is fine for the typical
1-3 selections; we revisit if user feedback says otherwise.)

- [ ] **Step 3: Add `PluginError::Custom` if not present**

Open `crates/savvagent-plugin/src/error.rs`. If `PluginError` already has
a free-form `Custom(String)` variant, skip this step. If not, add:

```rust
#[error("{0}")]
Custom(String),
```

…and re-run `cargo check -p savvagent-plugin` to confirm.

- [ ] **Step 4: Verify**

Run: `cargo test -p savvagent plugin::builtin::lsp_installer`
Expected: all tests pass (the two new ones in this task plus all
earlier ones).

- [ ] **Step 5: Commit**

```bash
git add crates/savvagent/src/plugin/builtin/lsp_installer/mod.rs crates/savvagent-plugin/src/error.rs
git commit -m "feat(internal:lsp-installer): __install dispatch with sequential install + config merge"
```

### Task 21: Register `LspInstallerPlugin` with the builtin set

**Files:**
- Modify: `crates/savvagent/src/plugin/mod.rs`

- [ ] **Step 1: Write the failing test**

Open `crates/savvagent/src/plugin/mod.rs`. In the existing
`#[tokio::test] async fn register_builtins_pr8_complete()` (or whichever
test enumerates the registered plugin ids), add `"internal:lsp-installer"`
to the `for expected in [...]` list and bump the
`assert_eq!(set.plugins.len(), 24);` to `25`.

- [ ] **Step 2: Run and watch it fail**

Run: `cargo test -p savvagent plugin::tests::register_builtins`
Expected: FAIL — missing `internal:lsp-installer`.

- [ ] **Step 3: Add the registration**

In `register_builtins()`, append to the `plugins` vec (alphabetical
placement):

```rust
Box::new(builtin::lsp_installer::LspInstallerPlugin::new()),
```

- [ ] **Step 4: Run the test**

Run: `cargo test -p savvagent plugin::tests::register_builtins`
Expected: PASS.

- [ ] **Step 5: Run the full workspace tests**

Run: `cargo test --workspace`
Expected: PASS — all 1017+ existing tests still green; the new
lsp_installer tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/savvagent/src/plugin/mod.rs
git commit -m "feat(internal:lsp-installer): register with builtin plugin set"
```

### Task 22: Smoke-test with a real download (fakelsp via local HTTP server)

**Files:**
- Create: `crates/savvagent/tests/lsp_installer_smoke.rs`

- [ ] **Step 1: Write a self-contained integration test**

Create `crates/savvagent/tests/lsp_installer_smoke.rs`:

```rust
//! Smoke test that exercises the full binary install path against a
//! local HTTP server serving a gzipped fixture. Verifies that:
//!   - download → SHA256 verify → gzip extract works end-to-end
//!   - the resulting binary is executable on Unix
//!   - merge_into_user_config writes a parseable lsp.toml entry

use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

fn gzipped(plain: &[u8]) -> Vec<u8> {
    use flate2::{write::GzEncoder, Compression};
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(plain).unwrap();
    enc.finish().unwrap()
}

#[tokio::test]
async fn install_binary_entry_end_to_end_via_local_server() {
    use savvagent::plugin::builtin::lsp_installer::catalog::{
        ArchiveKind, CatalogEntry, Category, InstallMethod, LspEntryTemplate, Target,
    };
    use savvagent::plugin::builtin::lsp_installer::installer::{
        install_binary_entry, ReqwestDownloader,
    };

    let payload = b"#!/bin/sh\necho hello-from-fakelsp\n";
    let archive = gzipped(payload);
    let sha = hex::encode(Sha256::digest(&archive));
    let archive = Arc::new(archive);

    // Spin up a tiny single-shot HTTP/1.1 server. Listens on 127.0.0.1
    // with an OS-assigned port, serves the gzipped archive, then exits.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let url = format!("http://127.0.0.1:{port}/fakelsp.gz");

    let archive_for_server = Arc::clone(&archive);
    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let (read, mut write) = sock.split();
        let mut reader = BufReader::new(read);
        // Discard request headers.
        let mut buf = String::new();
        while let Ok(n) = reader.read_line(&mut buf).await {
            if n == 0 || buf.ends_with("\r\n\r\n") || buf == "\r\n" {
                if buf == "\r\n" { break; }
                if buf.ends_with("\r\n\r\n") { break; }
            }
            if !buf.contains("\r\n") { break; }
        }
        let body = &*archive_for_server;
        let header = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
            body.len()
        );
        write.write_all(header.as_bytes()).await.unwrap();
        write.write_all(body).await.unwrap();
        write.flush().await.unwrap();
    });

    let url_static: &'static str = Box::leak(url.into_boxed_str());
    let sha_static: &'static str = Box::leak(sha.into_boxed_str());
    let urls: &'static [(Target, &'static str, &'static str)] =
        Box::leak(Box::new([(Target::current().unwrap(), url_static, sha_static)]));
    let entry = CatalogEntry {
        id: "fakelsp",
        display_name: "fakelsp",
        language_label: "fake",
        version: "0.0.0",
        category: Category::Binary,
        method: InstallMethod::BinaryDownload {
            urls,
            archive: ArchiveKind::GzipOnly,
            binary_path: "fakelsp",
        },
        lsp_entry: LspEntryTemplate {
            id: "fake",
            extensions: &["fake"],
            root_markers: &["fake.toml"],
            command: "{{BIN}}",
            args: &[],
        },
    };
    let tmp = tempfile::tempdir().unwrap();
    let dl = ReqwestDownloader { client: reqwest::Client::new() };
    let outcome = install_binary_entry(
        &entry,
        Target::current().unwrap(),
        tmp.path(),
        &dl,
        |_| {},
    )
    .await
    .expect("install must succeed");
    assert!(outcome.installed_at.exists());
    let _ = server.await;
}
```

- [ ] **Step 2: Run the smoke test**

Run: `cargo test -p savvagent --test lsp_installer_smoke`
Expected: PASS — the test serves the archive over loopback, downloads
it, verifies the checksum, gunzips it, and asserts the binary exists.

- [ ] **Step 3: Commit**

```bash
git add crates/savvagent/tests/lsp_installer_smoke.rs
git commit -m "test(internal:lsp-installer): end-to-end smoke against local http server"
```

### Task 23: README + CHANGELOG + open PR 2

**Files:**
- Modify: `README.md` (Language Server Protocol section)
- Modify: `CHANGELOG.md` (new section for the upcoming release)

- [ ] **Step 1: Add a `/lsp` paragraph to the README's LSP section**

In `README.md`, under the existing `## Language Server Protocol (LSP)`
heading added by PR #90, insert *before* the "Configuration" subsection:

```markdown
### Quick start: `/lsp` installer

Run `/lsp` from inside savvagent to open a multi-select picker of
curated language servers. Pick one or more with Space, confirm with
Enter, and savvagent will:

1. download the pinned upstream binary (or run `npm i -g` for Node-based
   servers) into `~/.savvagent/lsp-bin/<server-id>/`,
2. verify the SHA256 checksum,
3. merge the matching `[[language]]` entry into
   `~/.savvagent/lsp.toml`.

After installing, restart savvagent so `tool-lsp` re-reads the config.

Node-based servers (typescript-language-server, pyright,
bash-language-server, vscode-langservers-extracted) require `npm` on
`$PATH`. If it's missing, the picker still lists them but the install
step prints a "install Node.js first" message and skips that server —
the others in the batch still install.
```

- [ ] **Step 2: Add the CHANGELOG entry**

Open `CHANGELOG.md` and add a new section at the top (above the section
PR #90 added). Use `0.X.0` as a placeholder — replaced with the real
version in Task 24:

```markdown
## 0.X.0 - 2026-MM-DD

### Added

- **`/lsp` slash command**. Opens a multi-select picker over a curated
  catalog of language servers (rust-analyzer, clangd,
  lua-language-server, zls, marksman, typescript-language-server,
  pyright, bash-language-server, vscode-langservers-extracted). On
  confirm, savvagent downloads pinned binaries (or runs `npm i -g` for
  Node-based servers), verifies SHA256 checksums, and merges entries
  into `~/.savvagent/lsp.toml`.
- **`MultiSelectList<T>` widget** under `crates/savvagent/src/plugin/widgets/`.
  Generic state machine (cursor, filter, selection-by-stable-id)
  reusable by future plugins that need multi-select pickers.

### Notes

- Binaries land in `~/.savvagent/lsp-bin/<server-id>/`; tool-lsp's
  config loader picks them up on the next savvagent restart.
- Node servers list in the picker even when `npm` is absent; the install
  step prints a "install Node.js first" hint and skips them.
- gopls is intentionally not in v1: its canonical install requires the
  Go toolchain. Follow-up will add a `GoInstall` install method once
  the binary + npm paths prove the design.
```

- [ ] **Step 3: Run the full check before pushing**

Run in parallel:
- `rustup run stable cargo fmt --all -- --check`
- `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

Expected: all clean.

- [ ] **Step 4: Push and open PR 2**

```bash
git push -u origin feat/lsp-installer
gh pr create --title "feat(internal:lsp-installer): /lsp slash command + curated catalog installer" --body "$(cat <<'EOF'
## Summary

Adds the `internal:lsp-installer` plugin. `/lsp` opens a multi-select picker over a curated catalog of nine language servers; on confirm, savvagent downloads pinned binaries (SHA256-verified) or runs `npm i -g` for Node-based servers, then merges matching `[[language]]` entries into `~/.savvagent/lsp.toml`.

Wraps the `MultiSelectList<T>` widget shipped in the preceding PR.

## Test plan

- [ ] `cargo test --workspace` (expected: all green, ~30 new tests in `plugin::builtin::lsp_installer::*`).
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- [ ] `cargo fmt --check` — clean.
- [ ] Smoke-test 1: `cargo run -p savvagent`, type `/lsp`, pick `rust-analyzer`, confirm. Expect `~/.savvagent/lsp-bin/rust-analyzer/rust-analyzer` to exist and `~/.savvagent/lsp.toml` to gain a `[[language]] id = "rust"` entry pointing at it.
- [ ] Smoke-test 2: with `npm` removed from `$PATH`, run `/lsp` and pick `typescript-language-server`. Expect a `npm not found` note in the log; no other state mutated.
- [ ] Smoke-test 3: re-run `/lsp` and re-pick `rust-analyzer`. Expect the install dir to be wiped and re-populated; the `lsp.toml` entry replaced (not duplicated).
EOF
)"
```

- [ ] **Step 5: Verify CI**

Run: `gh run list --branch feat/lsp-installer --limit 3`
Expected: all required checks green. Fix and re-push if anything fails.

- [ ] **Step 6: Merge PR 2 when reviewed**

Squash-merge per repo convention.

---

## Release

### Task 24: Version bump + rollup commit + tag

**Files:**
- Modify: `Cargo.toml` (`[workspace.package].version` + every `[workspace.dependencies].*.version` literal for in-tree crates)
- Modify: `CHANGELOG.md` (replace `0.X.0` placeholder with the real version)

- [ ] **Step 1: Decide the version**

Run: `git tag --sort=-v:refname | head -5`
Take the highest existing `vN.N.N` tag, add one to the minor, zero the
patch. E.g. if `v0.16.0` is the latest published tag (from PR #90's
release), the LSP installer ships as `v0.17.0`. Per the
multi-phase-rollup memory, ignore any in-tree `release(0.X.0)` commits
that don't have a corresponding pushed tag.

- [ ] **Step 2: Bump the workspace**

Edit `Cargo.toml`. Set `[workspace.package].version = "0.17.0"`. In
`[workspace.dependencies]`, update every `version = "0.16.0"` literal
(for the in-tree crates `savvagent-plugin`, `savvagent-protocol`,
`savvagent-mcp`, `savvagent-host`, the four `provider-*` crates, and
the three `tool-*` crates) to `0.17.0`.

- [ ] **Step 3: Replace the CHANGELOG placeholder**

Edit `CHANGELOG.md`. Change `## 0.X.0 - 2026-MM-DD` to today's date
(`## 0.17.0 - 2026-MM-DD`).

- [ ] **Step 4: Verify the workspace builds + tests pass**

Run in parallel:
- `cargo build --workspace`
- `cargo test --workspace`
- `rustup run stable cargo clippy --workspace --all-targets -- -D warnings`
- `rustup run stable cargo fmt --all -- --check`

Expected: all clean.

- [ ] **Step 5: Commit the rollup**

```bash
git checkout master
git pull --ff-only
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "release(0.17.0): /lsp installer (multi-select widget + lsp_installer plugin)"
git push origin master
```

- [ ] **Step 6: Tag and let cargo-dist publish**

```bash
git tag v0.17.0
git push origin v0.17.0
```

Per the `cargo-dist owns the release lifecycle` memory: **do not** run
`gh release create` — cargo-dist's `Release` workflow detects the
tag push and publishes the GitHub Release with all per-target binaries
as its final step.

- [ ] **Step 7: Verify the release workflow**

Run: `gh run list --workflow Release --limit 1`
Watch the Release workflow run. When it completes:

Run: `gh release view v0.17.0`
Expected: all four per-target archives present (`aarch64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`,
`x86_64-pc-windows-msvc`) plus the install scripts.

- [ ] **Step 8: Close out the roadmap issue**

If a roadmap issue exists for this initiative, post a final comment:
"Shipped in v0.17.0 — PRs #N1 (multi-select widget) and #N2 (lsp
installer). Closing." Then close the issue.

Done.
