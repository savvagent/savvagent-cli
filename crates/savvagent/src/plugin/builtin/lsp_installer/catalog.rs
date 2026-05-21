//! Pinned LSP catalog (server id, version, download URLs, SHA256s).
//!
//! Versions + per-target SHA256s are pinned at publication time.
//! Refresh by editing this file when an upstream release lands.

/// One of the target triples we ship installers for. Matches the
/// cargo-dist `targets` list in `Cargo.toml`'s `[workspace.metadata.dist]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Target {
    /// `x86_64-unknown-linux-gnu`.
    LinuxX86_64Gnu,
    /// `aarch64-unknown-linux-gnu`.
    LinuxAarch64Gnu,
    /// `x86_64-apple-darwin`.
    MacosX86_64,
    /// `aarch64-apple-darwin`.
    MacosAarch64,
    /// `x86_64-pc-windows-msvc`.
    WindowsX86_64,
}

impl Target {
    /// Resolve the current host's target triple, or `None` if savvagent
    /// has no installer assets for this combination.
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

/// The `[[language]]` entry to merge into `~/.savvagent/lsp.toml` after
/// a successful install.
#[derive(Debug, Clone, Copy)]
pub struct LspEntryTemplate {
    /// Stable id, matches `tool_lsp::config::LanguageEntry::id`.
    pub id: &'static str,
    /// File extensions (no leading dot).
    pub extensions: &'static [&'static str],
    /// Root marker filenames.
    pub root_markers: &'static [&'static str],
    /// Executable command to write.
    pub command: CommandTemplate,
    /// Arguments passed to `command`.
    pub args: &'static [&'static str],
}

/// Where the `command` value in [`LspEntryTemplate`] comes from. Replaces
/// the earlier stringly-typed `"{{BIN}}"` sentinel with a closed enum so
/// typos can't slip past the type system.
#[derive(Debug, Clone, Copy)]
pub enum CommandTemplate {
    /// Write the absolute path to the binary the installer placed under
    /// `~/.savvagent/lsp-bin/<id>/`. Used by `BinaryDownload` entries
    /// whose `lsp.toml` entry needs the installer to fill in the path.
    Installed,
    /// Write this literal string into `command`. Used by `NpmGlobal`
    /// entries where the binary lands on `$PATH` and the literal name
    /// (e.g. `"typescript-language-server"`) is what `tool-lsp` should
    /// spawn.
    Literal(&'static str),
}

/// How [`super::installer`] should install a particular catalog entry.
/// The variant also drives the picker's category label.
#[derive(Debug, Clone, Copy)]
pub enum InstallMethod {
    /// Download from a templated URL, verify SHA256, extract. The
    /// extractor dispatches on each URL's actual suffix (`.gz`,
    /// `.tar.gz`, `.zip`), so a single entry can ship a `.gz` on Unix
    /// and a `.zip` on Windows without further annotation.
    BinaryDownload {
        /// One URL + checksum per supported `Target`.
        urls: &'static [(Target, &'static str, &'static str)],
        /// Relative path inside the extracted archive to the binary
        /// we'll point `lsp.toml` at, e.g. `"bin/lua-language-server"`.
        /// On Windows the installer appends `.exe` if missing.
        binary_path: &'static str,
    },
    /// `npm i -g <package>@<version>` (uses the host's npm).
    NpmGlobal {
        /// npm package name.
        package: &'static str,
        /// Binary that npm exposes after install — usually the same as
        /// `package` but sometimes different (e.g. pyright →
        /// `pyright-langserver`).
        binary: &'static str,
    },
}

impl InstallMethod {
    /// Picker label corresponding to the variant: `"binary"` for
    /// `BinaryDownload`, `"npm"` for `NpmGlobal`.
    pub fn category_label(&self) -> &'static str {
        match self {
            Self::BinaryDownload { .. } => "binary",
            Self::NpmGlobal { .. } => "npm",
        }
    }
}

/// A single catalog entry. `static CATALOG: &[CatalogEntry]` below
/// holds every server we ship installer support for.
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    /// Stable id, used in `/lsp __install <id>` and as the install-dir
    /// name under `~/.savvagent/lsp-bin/<id>/`.
    pub id: &'static str,
    /// Human-readable name shown in the picker.
    pub display_name: &'static str,
    /// Language label shown in the picker (`"rust"`, `"typescript"`, …).
    pub language_label: &'static str,
    /// Pinned upstream version.
    pub version: &'static str,
    /// How to install.
    pub method: InstallMethod,
    /// What to write into `lsp.toml` after a successful install.
    pub lsp_entry: LspEntryTemplate,
}

/// Pinned v1 catalog. SHA256s come from GitHub's release `assets[].digest`
/// field (`sha256:` prefix stripped). Versions and checksums are refreshed
/// by editing this file when an upstream release lands; the spec
/// (`docs/superpowers/specs/2026-05-20-lsp-installer-design.md`) covers
/// the update workflow in full.
///
/// **Servers not in the catalog**, with the structural change each would
/// require:
///
/// - **clangd** — ships only monolithic linux/mac/windows zips, no
///   separate aarch64 builds. Needs a per-target asset list (some
///   `Target` variants missing) before it can fit.
/// - **zls** — Unix releases use `.tar.xz`. Needs an `xz2`-backed
///   decompressor path in `installer::extract_one`.
/// - **marksman** — ships raw, unwrapped executables (no archive). Needs
///   a `Raw` extract path that writes the downloaded bytes verbatim.
pub static CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        id: "rust-analyzer",
        display_name: "rust-analyzer",
        language_label: "rust",
        version: "2026-05-18",
        method: InstallMethod::BinaryDownload {
            urls: &[
                (
                    Target::LinuxX86_64Gnu,
                    "https://github.com/rust-lang/rust-analyzer/releases/download/2026-05-18/rust-analyzer-x86_64-unknown-linux-gnu.gz",
                    "d4a0f9acec52904af584332199cb41d19c9d5d4bf0d15e0466459c1d1b2d8881",
                ),
                (
                    Target::LinuxAarch64Gnu,
                    "https://github.com/rust-lang/rust-analyzer/releases/download/2026-05-18/rust-analyzer-aarch64-unknown-linux-gnu.gz",
                    "7ff2062959cff408fb1c6e73f842c62896a3e808f83a2781a1b8f38a764df7d0",
                ),
                (
                    Target::MacosX86_64,
                    "https://github.com/rust-lang/rust-analyzer/releases/download/2026-05-18/rust-analyzer-x86_64-apple-darwin.gz",
                    "185f13571ad0b092475e7b8be79587227e6f0749505a8cde1c8443d596c3e349",
                ),
                (
                    Target::MacosAarch64,
                    "https://github.com/rust-lang/rust-analyzer/releases/download/2026-05-18/rust-analyzer-aarch64-apple-darwin.gz",
                    "3753cd9f0ee40914b2f9e51475b8df7aba74e034aca5175ffeb4a53c6aaf53f3",
                ),
                (
                    Target::WindowsX86_64,
                    "https://github.com/rust-lang/rust-analyzer/releases/download/2026-05-18/rust-analyzer-x86_64-pc-windows-msvc.zip",
                    "9990c96852ba745ad555404747ce3cb1be29d461f968ba68c8316f64452f6a47",
                ),
            ],

            binary_path: "rust-analyzer",
        },
        lsp_entry: LspEntryTemplate {
            id: "rust",
            extensions: &["rs"],
            root_markers: &["Cargo.toml", "rust-project.json"],
            command: CommandTemplate::Installed,
            args: &[],
        },
    },
    CatalogEntry {
        id: "lua-language-server",
        display_name: "lua-language-server",
        language_label: "lua",
        version: "3.18.2",
        method: InstallMethod::BinaryDownload {
            urls: &[
                (
                    Target::LinuxX86_64Gnu,
                    "https://github.com/LuaLS/lua-language-server/releases/download/3.18.2/lua-language-server-3.18.2-linux-x64.tar.gz",
                    "ca71415dd19f19e30aaa35a4915aefca9fdb5fec31b98331cc3d77f778d539c5",
                ),
                (
                    Target::LinuxAarch64Gnu,
                    "https://github.com/LuaLS/lua-language-server/releases/download/3.18.2/lua-language-server-3.18.2-linux-arm64.tar.gz",
                    "273af33f26f4a1143f27c96d9f9e1188aba619c71e0807042134f66b4bd27f24",
                ),
                (
                    Target::MacosX86_64,
                    "https://github.com/LuaLS/lua-language-server/releases/download/3.18.2/lua-language-server-3.18.2-darwin-x64.tar.gz",
                    "e26cfefe423dd7326fc7c649539e4d4aaa4f35f34d2fefd8af2ed7090b72c556",
                ),
                (
                    Target::MacosAarch64,
                    "https://github.com/LuaLS/lua-language-server/releases/download/3.18.2/lua-language-server-3.18.2-darwin-arm64.tar.gz",
                    "cec99d70b1f612acec4a10a79a03664e3aa0c229d4d8a586cb3f928ec37d509e",
                ),
                (
                    Target::WindowsX86_64,
                    "https://github.com/LuaLS/lua-language-server/releases/download/3.18.2/lua-language-server-3.18.2-win32-x64.zip",
                    "a4439a8f5e8e9e6505c11f045a7bf45db602124a1e246371c1dbe34924f3cf71",
                ),
            ],

            binary_path: "bin/lua-language-server",
        },
        lsp_entry: LspEntryTemplate {
            id: "lua",
            extensions: &["lua"],
            root_markers: &[".luarc.json", ".luarc.jsonc"],
            command: CommandTemplate::Installed,
            args: &[],
        },
    },
    CatalogEntry {
        id: "typescript-language-server",
        display_name: "typescript-language-server",
        language_label: "typescript",
        version: "5.3.0",
        method: InstallMethod::NpmGlobal {
            package: "typescript-language-server",
            binary: "typescript-language-server",
        },
        lsp_entry: LspEntryTemplate {
            id: "typescript",
            extensions: &["ts", "tsx", "mts", "cts"],
            root_markers: &["tsconfig.json", "package.json"],
            command: CommandTemplate::Literal("typescript-language-server"),
            args: &["--stdio"],
        },
    },
    CatalogEntry {
        id: "pyright",
        display_name: "pyright",
        language_label: "python",
        version: "1.1.409",
        method: InstallMethod::NpmGlobal {
            package: "pyright",
            binary: "pyright-langserver",
        },
        lsp_entry: LspEntryTemplate {
            id: "python",
            extensions: &["py"],
            root_markers: &["pyproject.toml", "setup.py", "pyrightconfig.json"],
            command: CommandTemplate::Literal("pyright-langserver"),
            args: &["--stdio"],
        },
    },
    CatalogEntry {
        id: "bash-language-server",
        display_name: "bash-language-server",
        language_label: "bash",
        version: "5.6.0",
        method: InstallMethod::NpmGlobal {
            package: "bash-language-server",
            binary: "bash-language-server",
        },
        lsp_entry: LspEntryTemplate {
            id: "bash",
            extensions: &["sh", "bash"],
            root_markers: &[".bashrc"],
            command: CommandTemplate::Literal("bash-language-server"),
            args: &["start"],
        },
    },
    CatalogEntry {
        id: "vscode-langservers-extracted",
        display_name: "vscode-langservers-extracted",
        language_label: "html",
        version: "4.10.0",
        method: InstallMethod::NpmGlobal {
            package: "vscode-langservers-extracted",
            binary: "vscode-html-language-server",
        },
        lsp_entry: LspEntryTemplate {
            id: "html",
            extensions: &["html"],
            root_markers: &["package.json"],
            command: CommandTemplate::Literal("vscode-html-language-server"),
            args: &["--stdio"],
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_current_returns_some_on_supported_host() {
        assert!(
            Target::current().is_some(),
            "expected supported host target on CI runners"
        );
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = CATALOG.iter().map(|e| e.id).collect();
        ids.sort();
        let len_before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), len_before, "duplicate ids in CATALOG: {:?}", ids);
    }

    #[test]
    fn binary_entries_cover_every_target() {
        for entry in CATALOG {
            if let InstallMethod::BinaryDownload { urls, .. } = entry.method {
                let mut covered: Vec<String> =
                    urls.iter().map(|(t, _, _)| format!("{t:?}")).collect();
                covered.sort();
                covered.dedup();
                assert_eq!(
                    covered.len(),
                    5,
                    "{}: must list one URL per Target variant (got {:?})",
                    entry.id,
                    covered
                );
            }
        }
    }
}
