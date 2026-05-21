# `/lsp` Installer Design

**Status:** Drafted 2026-05-20. Blocked on PR #90 (`feat/host-resources-and-tool-lsp`) merging — installer writes `lsp.toml` entries the `tool-lsp` config loader added in that PR will consume.

**Owner area:** `crates/savvagent/src/plugin/builtin/lsp_installer/` (new), `crates/savvagent/src/plugin/widgets/multi_select_list.rs` (new).

## Goals

1. Ship a `/lsp` slash command that opens a multi-select picker listing curated language-server entries (name, language, install method, install status).
2. Let the user pick one or more, confirm, and have savvagent download or install the binaries with no further intervention.
3. Auto-merge the corresponding entries into `~/.savvagent/lsp.toml` so the next time `tool-lsp` spawns, the LSPs are usable.
4. Surface install progress and final status in the conversation log so the user sees what happened.

## Non-Goals

- **Auto-updating** installed LSPs. v1 installs a pinned version and stops; users re-run `/lsp` to upgrade. The picker shows the installed version vs. the catalog version side-by-side so the choice is obvious.
- **Uninstalling.** Users delete `~/.savvagent/lsp-bin/<id>/` manually and edit `lsp.toml`. Add a `/lsp remove` follow-up if anyone asks.
- **Per-project install isolation.** Binaries are global at `~/.savvagent/lsp-bin/`; `lsp.toml` resolution is the only place per-project overrides apply (already handled by the loader from PR #90).
- **Installing language toolchains.** If `npm` isn't on `$PATH`, we bail with a clear message and do not attempt to install Node.
- **Discovering already-installed system LSPs.** We don't probe `$PATH` for pre-existing `rust-analyzer` etc.; the user explicitly opts in via the picker. Out-of-band installs are still respected by `tool-lsp`'s `command` resolution.

## UX Sketch

```
/lsp <Enter>
```

opens a fullscreen modal:

```
Filter: _

Select language servers to install. Space toggles, Enter confirms, Esc cancels.

  Selected: 2

  Binary downloads
  [x] rust-analyzer           rust          2025.04.21    (not installed)
  [ ] clangd                  c/c++         19.1.0        (installed: 19.1.0)
  [x] lua-language-server     lua           3.13.2        (not installed)
> [ ] zls                     zig           0.13.0        (not installed)
  [ ] marksman                markdown      2024-12-18    (not installed)

  Node.js (requires npm)
  [ ] typescript-language-server  typescript  4.3.3       (not installed)
  [ ] pyright                     python      1.1.385     (not installed)
  [ ] bash-language-server        bash        5.4.4       (not installed)
  [ ] vscode-langservers-extracted html/css/json 4.10.0   (not installed)
```

On Enter with `n` items checked, the picker closes and the conversation log shows:

```
[lsp-installer] installing 2 servers…
[lsp-installer] rust-analyzer 2025.04.21: downloading…
[lsp-installer] lua-language-server 3.13.2: downloading…
[lsp-installer] rust-analyzer 2025.04.21: extracted to ~/.savvagent/lsp-bin/rust-analyzer/rust-analyzer
[lsp-installer] lua-language-server 3.13.2: extracted to ~/.savvagent/lsp-bin/lua-language-server/bin/lua-language-server
[lsp-installer] wrote 2 entries to ~/.savvagent/lsp.toml
[lsp-installer] done — restart savvagent or run /reload to pick up the new servers
```

If `npm` isn't found and a Node-based server was selected:

```
[lsp-installer] typescript-language-server: npm not found on $PATH
                install Node.js from https://nodejs.org and re-run /lsp
```

## Architecture

### PR 1 — Reusable multi-select widget

A standalone state-machine helper lives at
`crates/savvagent/src/plugin/widgets/multi_select_list.rs`. It is **not** a
`Plugin` or `Screen` — it is a generic struct that any future `Screen` impl
can wrap. This mirrors the existing `themes::picker::ThemePicker` /
`themes::screen::ThemePickerScreen` split, with the picker state generalised
to "list of `T`" and the outcome enum extended with `Toggle` and `Confirm`.

```rust
pub struct MultiSelectList<T> {
    items: Vec<T>,
    filter: String,
    cursor: usize,                  // index into the filtered view
    selected_ids: BTreeSet<String>, // stable across filter changes
    filter_fn: Box<dyn Fn(&T, &str) -> bool + Send>,
    id_fn:     Box<dyn Fn(&T) -> String + Send>,
}

pub enum MultiSelectOutcome<T: Clone> {
    Stay,
    Preview(T),
    Toggle(T),       // selection state for cursor item flipped
    Confirm(Vec<T>), // returns the selected items in catalog order
    Cancel,
}
```

Keybindings (handled by `on_key(crossterm::event::KeyEvent) -> MultiSelectOutcome<T>`):

| Key            | Behaviour                                                       |
| -------------- | --------------------------------------------------------------- |
| `Up` / `Down`  | Move cursor in filtered view; emits `Preview` for new row.      |
| `Space`        | Toggle selection for item under cursor; emits `Toggle`.         |
| `Enter`        | `Confirm` with the current selected set (in catalog order).     |
| `Esc`          | `Cancel`.                                                       |
| `Backspace`    | Pop last filter char; re-clamp cursor; emits `Preview`.         |
| Printable char | Append to filter; re-clamp cursor; emits `Preview`.             |

**Why selection by string id, not index?** The filter narrows the list but
must not lose previously-selected items. Tracking by stable id (`id_fn`)
means a selection survives the user typing into and out of the filter, and
survives the same id appearing at a different filtered index. Confirm walks
`items` (not the filtered view) so the returned `Vec<T>` is in catalog
order regardless of selection sequence.

No new types on the `savvagent-plugin` trait surface — this is a private
helper inside the `savvagent` crate.

### PR 2 — `/lsp` plugin

New module `crates/savvagent/src/plugin/builtin/lsp_installer/` with:

```
lsp_installer/
├── mod.rs           # Plugin impl, slash registration, install task dispatch
├── catalog.rs       # static `CATALOG: &[CatalogEntry]` + types
├── installer.rs     # per-entry install: download/verify/extract OR `npm i -g`
├── config_writer.rs # merge catalog → lsp.toml entries
├── picker.rs        # wraps MultiSelectList<&'static CatalogEntry>
└── screen.rs        # LspPickerScreen (Screen impl)
```

The plugin manifest:

- `id = "internal:lsp-installer"`
- `kind = PluginKind::Optional`
- One slash: `/lsp` (no args) → opens picker.
- One screen: `"lsp_installer.picker"` → constructed via `create_screen`.
- Hooks: none. The install task is spawned synchronously inside `handle_slash`
  for `__install` (a private sub-command the picker uses internally).

#### Catalog entry

```rust
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub id: &'static str,          // e.g. "rust-analyzer" — also tool-lsp `language.id`
    pub display_name: &'static str,
    pub language_label: &'static str,
    pub version: &'static str,     // pinned
    pub category: Category,        // Binary | Npm
    pub method: InstallMethod,
    /// What to write into `lsp.toml` once the install succeeds. `command`
    /// is left as `{{BIN}}` and substituted with the absolute installed
    /// path at write time.
    pub lsp_entry: LspEntryTemplate,
}

pub enum Category { Binary, Npm }

pub enum InstallMethod {
    /// Direct download from a templated URL. `{{target}}` is replaced
    /// with the rust target triple (or a per-asset mapping); the
    /// resulting bytes are checksummed against `sha256`, then unpacked
    /// according to `archive`.
    BinaryDownload {
        urls: &'static [(Target, &'static str, &'static str)], // (target, url, sha256-hex)
        archive: ArchiveKind,
        /// Relative path inside the extracted archive to the binary
        /// we'll point `lsp.toml` at. e.g. `"bin/lua-language-server"`.
        binary_path: &'static str,
    },
    /// `npm i -g <package>@<version>`. We do not download anything
    /// ourselves; we shell out to the user's npm.
    NpmGlobal {
        package: &'static str,
        /// Binary name produced by the npm package (looked up via
        /// `npm bin -g` or hardcoded — usually the same as `package`
        /// or a sibling, e.g. `pyright-langserver`).
        binary: &'static str,
    },
}

pub enum ArchiveKind { GzipOnly, TarGz, Zip }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    LinuxX86_64Gnu,
    LinuxAarch64Gnu,
    MacosX86_64,
    MacosAarch64,
    WindowsX86_64,
}

pub struct LspEntryTemplate {
    pub id: &'static str,                    // tool-lsp `language.id`
    pub extensions: &'static [&'static str],
    pub root_markers: &'static [&'static str],
    /// May be `"{{BIN}}"` (substituted with installed path) or a literal
    /// like `"typescript-language-server"` (when npm puts it on $PATH).
    pub command: &'static str,
    pub args: &'static [&'static str],
}
```

The full v1 catalog (~9 entries) is declared as `static CATALOG: &[CatalogEntry]`
in `catalog.rs`. Pinned versions + per-target SHA256s live alongside the URL
template:

```rust
static CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "rust-analyzer",
        display_name: "rust-analyzer",
        language_label: "rust",
        version: "2025.04.21",
        category: Category::Binary,
        method: InstallMethod::BinaryDownload {
            urls: &[
                (Target::LinuxX86_64Gnu,  "https://github.com/.../rust-analyzer-x86_64-unknown-linux-gnu.gz", "abc123…"),
                (Target::LinuxAarch64Gnu, "https://.../rust-analyzer-aarch64-unknown-linux-gnu.gz", "def456…"),
                (Target::MacosX86_64,     "https://.../rust-analyzer-x86_64-apple-darwin.gz", "…"),
                (Target::MacosAarch64,    "https://.../rust-analyzer-aarch64-apple-darwin.gz", "…"),
                (Target::WindowsX86_64,   "https://.../rust-analyzer-x86_64-pc-windows-msvc.zip", "…"),
            ],
            archive: ArchiveKind::GzipOnly, // (Zip on windows; switch handled at install time)
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
    // … clangd, lua-language-server, zls, marksman …
    // … typescript-language-server, pyright, bash-language-server, vscode-langservers-extracted …
];
```

Initial catalog (will be filled with real version strings and checksums
during PR 2 implementation; the plan's smoke-test task verifies that the
recorded SHA256s match the actual upstream artifacts on the day the PR is
written):

| id                              | category | version    | install method        | binary on PATH after install                          |
| ------------------------------- | -------- | ---------- | --------------------- | ----------------------------------------------------- |
| rust-analyzer                   | Binary   | 2025.04.21 | gzip / zip            | `~/.savvagent/lsp-bin/rust-analyzer/rust-analyzer`    |
| clangd                          | Binary   | 19.1.0     | zip                   | `~/.savvagent/lsp-bin/clangd/bin/clangd`              |
| lua-language-server             | Binary   | 3.13.2     | tar.gz / zip          | `~/.savvagent/lsp-bin/lua-language-server/bin/lua-language-server` |
| zls                             | Binary   | 0.13.0     | tar.gz / zip          | `~/.savvagent/lsp-bin/zls/zls`                        |
| marksman                        | Binary   | 2024-12-18 | gzip / zip            | `~/.savvagent/lsp-bin/marksman/marksman`              |
| typescript-language-server      | Npm      | 4.3.3      | `npm i -g`            | `typescript-language-server` (npm global bin)         |
| pyright                         | Npm      | 1.1.385    | `npm i -g`            | `pyright-langserver`                                  |
| bash-language-server            | Npm      | 5.4.4      | `npm i -g`            | `bash-language-server`                                |
| vscode-langservers-extracted    | Npm      | 4.10.0     | `npm i -g`            | `vscode-html-language-server` (one of several)        |

gopls is **deliberately out of v1**: its canonical install path is
`go install …`, which requires the Go toolchain. Adding a third install
method (CargoInstall / GoInstall) would balloon the design. We can ship a
follow-up that adds `GoInstall` once the Binary + Npm paths prove the
pattern.

#### Install state machine

`InstallMethod` is interpreted by `installer.rs::install_entry`:

```rust
pub async fn install_entry(
    entry: &CatalogEntry,
    lsp_bin_root: &Path,            // ~/.savvagent/lsp-bin/
    npm_path: Option<&Path>,        // pre-resolved by detect_npm()
    notify: impl Fn(InstallProgress),
) -> Result<InstallOutcome, InstallError>;
```

Stages emitted via `notify`:

| Stage              | Notes                                                            |
| ------------------ | ---------------------------------------------------------------- |
| `Started`          | Picker confirm received; fired once per entry.                   |
| `Downloading { bytes_so_far, total }` | Only for `BinaryDownload`.                |
| `Verifying`        | SHA256 hashing.                                                  |
| `Extracting`       | gunzip / untar / unzip.                                          |
| `RunningNpm`       | Only for `NpmGlobal`. Streams npm's stdout per-line as further notes. |
| `Done { installed_at: PathBuf }` | Terminal success.                              |
| `Failed { reason: String }`      | Terminal failure (does not abort the batch).   |

The picker confirm path emits a single `Effect::RunSlash` re-entering
`handle_slash("lsp", ["__install", "rust", "go", …])`. That handler spawns
one `tokio::task` per selected entry that calls `install_entry` and pushes
notes back to the conversation log via the `App::push_note` channel
(`Effect::PushNote` is the existing primitive). When all tasks finish, a
single `Effect::PushNote` summarising the batch is emitted plus a
`config_writer::merge_into_user_config(&[outcomes])` call.

We deliberately do **not** add new `Effect` or `HostEvent` variants for
install progress. `PushNote` is already the right shape for "stream lines
into the log," matching how `internal:self-update`'s `/update` path
surfaces its progress today.

**Concurrency cap.** The batch runs up to four installs in parallel via
`futures::stream::FuturesUnordered` — enough to overlap network latency,
small enough that npm installs don't fight over the registry. The cap is
a constant in `mod.rs`; not surfaced as a config.

#### Config merge

`config_writer.rs` reads `~/.savvagent/lsp.toml` if present, parses it
into a `serde_json::Value`-tolerant intermediate (so we don't drop
unknown future fields), upserts each new entry by `language.id`, and
writes back via `toml::to_string_pretty`. The merge:

- replaces any entry with the same `id` (matching tool-lsp's
  repo-replaces-global semantics, applied here to user-vs-installer);
- preserves order: existing entries keep their relative position; new
  entries append at the bottom;
- is atomic on Unix via write-to-temp + `rename`. On Windows we use the
  same pattern (best-effort; `tempfile::persist` handles the platform
  differences).

The first install in any session creates `~/.savvagent/lsp.toml` if
it doesn't exist.

#### Reload semantics

`tool-lsp` reads `lsp.toml` once at child-process startup. Updating the
file *after* the LSP tool child has started doesn't take effect until the
tool restarts. v1 prints a "restart savvagent or run /reload" hint after
each successful install batch. A `/reload` command (re-spawning tool-lsp)
is **out of scope** for this initiative — it should land as part of a
broader "reload tools" affordance later.

### Filesystem layout

```
~/.savvagent/
├── lsp.toml           # written/updated by config_writer.rs
└── lsp-bin/
    ├── rust-analyzer/
    │   └── rust-analyzer            (or .exe on Windows)
    ├── clangd/
    │   └── bin/clangd
    ├── lua-language-server/
    │   ├── bin/lua-language-server
    │   └── main.lua                  (rest of the archive — left as-is)
    └── zls/
        └── zls
```

The installer creates `~/.savvagent/lsp-bin/<id>/`, downloads into it,
extracts in place, sets the executable bit on Unix, and uses
`binary_path` (joined to the install dir) as the absolute path written
into `lsp.toml`'s `command` field.

### Dependencies

Already in workspace; new module reuses them:

- `reqwest` (HTTP). Already pinned with rustls + http2 + stream features.
- `tokio` (spawn, async io).
- `sha2` — **not yet in workspace.** Added to `[workspace.dependencies]`
  as `sha2 = "0.10"` plus an entry in the lsp_installer's `Cargo.toml`-equivalent
  (which is the `savvagent` crate's `[dependencies]`, since the plugin is
  in-tree).
- `flate2` (gzip), `tar`, `zip` — **not yet in workspace.** Added to
  `[workspace.dependencies]`. `flate2 = "1"`, `tar = "0.4"`, `zip = "5"`.
  The `self_update` crate already pulls `flate2` transitively, but its
  internals aren't a public API, so we add a direct dep rather than
  reaching through.

No new deps for PR 1 (the widget uses only `std` + existing
`crossterm` types via the same conversion path that themes/picker uses).

### Tests

**PR 1:**

- `MultiSelectList::new` populates items, empty filter, cursor=0, no selection.
- Cursor up/down clamps at filtered boundaries; emits `Preview` for the
  new row.
- Filter typing narrows the visible set; cursor clamps if the filtered
  list shrinks past it.
- Selection persists across filter changes: select item A, type a filter
  that hides A, type backspace to restore, confirm — A is in the result.
- Confirm with empty selection returns `Confirm(vec![])` (callers decide
  whether to act on that — for `/lsp`, the screen treats empty Confirm as
  Cancel; but the widget itself doesn't enforce non-empty).
- Confirm returns items in **catalog order**, not selection order.
- Toggle on cursor row flips `selected_ids`; second Toggle removes.
- Esc emits `Cancel` with no mutation.

**PR 2:**

- `catalog::CATALOG` has at least 9 entries; every entry's
  `LspEntryTemplate.id` is unique.
- For each binary entry, every `Target` variant has a corresponding URL.
- `installer::install_entry` happy path: a stubbed `reqwest::Client`
  returns a known gzipped payload; the entry's `sha256` is set to the
  hash of that payload; the install succeeds and writes the binary at
  the expected path with the executable bit set on Unix.
- `installer::install_entry` checksum-mismatch path: returns
  `InstallError::ChecksumMismatch`; the file is *not* left in place.
- `installer::install_entry` npm path: stubs `Command::new` via a thin
  trait wrapper; verifies the right argv is constructed and stdout lines
  are forwarded.
- `installer::install_entry` npm-missing path: returns
  `InstallError::ToolNotFound { tool: "npm" }`.
- `config_writer::merge_into_user_config`: roundtrips an existing
  `lsp.toml` with two entries, adds one new + replaces one, asserts
  ordering and that unknown future fields are preserved.
- `lsp_installer::LspPickerScreen` integration: open → toggle two
  entries → Enter → emits `Stack(vec![CloseScreen, RunSlash { name:
  "lsp", args: vec!["__install", <id1>, <id2>] }])`.
- `lsp_installer::handle_slash("lsp", ["__install", ...])` with a
  stubbed installer: spawns N tasks, eventually emits the summary
  PushNote with success/failure counts.

### Risks & open questions

1. **Checksum bit-rot.** Upstream projects re-upload assets occasionally
   (rare, but does happen for prebuilt LSPs when a hotfix lands). When a
   checksum diverges the install bails clearly. The cure is a catalog
   bump in our repo — same workflow as the `cargo-dist` versioning we
   already maintain.
2. **GitHub rate limits.** Direct downloads from `github.com/.../releases/download/` use the asset CDN and aren't subject to API rate limits, but we still set a `User-Agent: savvagent/<version>` header.
3. **Windows binary names.** Each catalog entry needs the `.exe` suffix
   for `binary_path` when `Target::WindowsX86_64`. Spec choice: store a
   single `binary_path` and append `.exe` on Windows in `install_entry`
   rather than complicating the schema.
4. **npm global bin location.** `npm bin -g` was removed in npm 9. We
   resolve the installed binary path via `npm root -g` + a hardcoded
   per-entry binary name. Tested in CI by mocking npm.
5. **Re-install behaviour.** If the install dir already exists, we wipe
   it (`tokio::fs::remove_dir_all`) before extracting. This means
   `/lsp` is idempotent and acts as both install and upgrade. Captured
   in the test plan.
6. **Permissions.** We write to `~/.savvagent/lsp-bin/` and require
   `chmod +x` on Unix. We do not need sudo. If `~/.savvagent/` is
   non-writable we emit a `Failed` stage and the batch carries on.

## Release shape

Per the multi-phase-rollup convention, both PRs land on `master` with
`release(0.X.0)` *scaffolding* commits; no tag is pushed mid-series. Once
PR 2 merges and a quick `cargo test --workspace` + clippy + fmt clears
on master, we:

1. Bump `[workspace.package].version` and every `[workspace.dependencies].*.version` literal to `0.X.0` (X = next minor after whatever PR #90 publishes).
2. Add a `## 0.X.0 - 2026-MM-DD` CHANGELOG section covering both PRs.
3. Update README's "Language Server Protocol" section with a short paragraph on `/lsp` (paired example).
4. Commit `release(v0.X.0): /lsp installer (multi-select widget + lsp_installer plugin)`.
5. Push the `v0.X.0` tag. cargo-dist takes over.

The keep-issue-current memory applies: the LSP installer roadmap issue
(to be opened before PR 1) gets a comment for each PR and a final
"shipped in v0.X.0" close comment.
