//! Single-pass templating expansion for command bodies.

/// Outcome of expanding a command body.
#[derive(Debug, Clone, Default)]
pub struct Expanded {
    /// The rendered prompt text.
    pub text: String,
    /// Non-fatal warnings emitted during expansion.
    pub warnings: Vec<String>,
}

/// Expand `@<path>` tokens by inlining the file contents.
///
/// Token shape: `@` (in a valid position — start of string, after
/// whitespace, or after one of `( [ { , ' "`) followed by a contiguous
/// run of non-whitespace characters.
///
/// Missing files leave the literal `@<path>` in place and emit a
/// warning. Single-pass: included files are NOT re-expanded.
pub fn expand_files(body: &str) -> Expanded {
    let mut out = String::with_capacity(body.len());
    let mut warnings = Vec::new();
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c != b'@' {
            // Push one UTF-8 char.
            let ch_end = utf8_char_end(bytes, i);
            out.push_str(&body[i..ch_end]);
            i = ch_end;
            continue;
        }
        // Position check: previous byte must be absent, whitespace,
        // newline, or one of ( [ { , ' "
        let valid_position = i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'\n' | b'\r' | b'(' | b'[' | b'{' | b',' | b'\'' | b'"'
            );
        if !valid_position {
            out.push('@');
            i += 1;
            continue;
        }
        // Find the path: from i+1 until next whitespace or EOF.
        let start = i + 1;
        let mut end = start;
        while end < bytes.len() && !bytes[end].is_ascii_whitespace() {
            end = utf8_char_end(bytes, end);
        }
        let path = &body[start..end];
        if path.is_empty() {
            out.push('@');
            i += 1;
            continue;
        }
        match std::fs::read_to_string(path) {
            Ok(contents) => out.push_str(&contents),
            Err(_) => {
                warnings.push(format!("@{path}: file not found"));
                out.push('@');
                out.push_str(path);
            }
        }
        i = end;
    }
    Expanded {
        text: out,
        warnings,
    }
}

/// Return the byte index just past the UTF-8 character starting at `i`.
fn utf8_char_end(bytes: &[u8], i: usize) -> usize {
    let b = bytes[i];
    let len = if b & 0x80 == 0 {
        1
    } else if b & 0xE0 == 0xC0 {
        2
    } else if b & 0xF0 == 0xE0 {
        3
    } else {
        4
    };
    (i + len).min(bytes.len())
}

/// Substitute `$ARGUMENTS` and `$1`/`$2`/… in `body`.
///
/// `$ARGUMENTS` becomes the raw argument string (`args.join(" ")`).
/// `$N` substitutions reference whitespace-split positional args;
/// out-of-range positions expand to the empty string. Supports `$1`
/// through `$9` only; multi-digit positions (`$10`+) are out of scope
/// for v1.
pub fn expand_args(body: &str, args: &[String]) -> String {
    let raw = args.join(" ");
    let mut out = body.replace("$ARGUMENTS", &raw);
    // Replace $1..$9 in argument order; values not present become empty.
    for (idx, a) in args.iter().take(9).enumerate() {
        out = out.replace(&format!("${}", idx + 1), a);
    }
    // Blank out any remaining $N referring to out-of-range positions.
    for n in (args.len() + 1)..=9 {
        out = out.replace(&format!("${n}"), "");
    }
    out
}

use tokio::process::Command;

/// Expand `!<cmd>` tokens by running the shell and inlining stdout.
///
/// Two forms accepted:
///
/// - **Line-leading:** A line whose first non-whitespace char is `!`
///   treats the rest of the line as the command.
/// - **Inline backtick:** `` !` `` followed by the command followed by
///   `` ` `` substitutes stdout in place.
///
/// Non-zero exit aborts expansion and returns `Err(summary)`. The
/// caller surfaces the error in the conversation log and does NOT
/// submit the prompt. The shell is `sh -c <cmd>`.
pub async fn expand_shell(body: &str) -> Result<Expanded, String> {
    let mut warnings: Vec<String> = Vec::new();

    // Pass 1: inline backtick form `!`...`
    let mut after_inline = String::new();
    let mut cursor = 0usize;
    while let Some(pos) = body[cursor..].find("!`") {
        let abs = cursor + pos;
        after_inline.push_str(&body[cursor..abs]);
        let cmd_start = abs + 2;
        let Some(close) = body[cmd_start..].find('`') else {
            // Unmatched backtick — leave the rest as-is, emit a warning,
            // and stop the pass.
            after_inline.push_str(&body[abs..]);
            cursor = body.len();
            warnings.push("unmatched backtick after `!\\``; leaving as literal".to_string());
            break;
        };
        let cmd = &body[cmd_start..cmd_start + close];
        let stdout = run_shell(cmd).await?;
        // Trim a single trailing newline (with optional \r) so the
        // substitution doesn't split surrounding text onto a new line.
        let trimmed = stdout
            .strip_suffix('\n')
            .map(|s| s.strip_suffix('\r').unwrap_or(s))
            .unwrap_or(&stdout);
        after_inline.push_str(trimmed);
        cursor = cmd_start + close + 1;
    }
    after_inline.push_str(&body[cursor..]);

    // Pass 2: line-leading `!cmd` form (whitespace before `!` allowed).
    let mut final_out = String::new();
    for line in after_inline.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('!') {
            let cmd = rest.trim_end_matches('\n').trim_end_matches('\r');
            if cmd.is_empty() {
                final_out.push_str(line);
                continue;
            }
            let stdout = run_shell(cmd).await?;
            final_out.push_str(&stdout);
            if !stdout.ends_with('\n') && line.ends_with('\n') {
                final_out.push('\n');
            }
        } else {
            final_out.push_str(line);
        }
    }
    Ok(Expanded {
        text: final_out,
        warnings,
    })
}

use crate::plugin::builtin::user_slash_commands::trust::TrustLevel;

/// Returns `true` if the body contains any `!<cmd>` token. Used by the
/// dispatcher to decide whether to invoke the trust check.
///
/// Recognizes:
/// - Line-leading `!` (after any leading whitespace).
/// - Inline backtick form `!\``.
///
/// Conservatively avoids matching `!=` and `!!` as shell tokens.
pub fn contains_shell_token(body: &str) -> bool {
    if body.contains("!`") {
        return true;
    }
    for line in body.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('!') {
            if !rest.starts_with('=') && !rest.starts_with('!') {
                return true;
            }
        }
    }
    false
}

/// Run all expansion passes in order: `$ARGUMENTS`/`$N` → `@<path>` →
/// `!<cmd>`. Single-pass: included files are NOT re-expanded.
///
/// `trust` gates shell substitution:
/// - `Some(TrustLevel::Always)`: shell substitution runs.
/// - `Some(TrustLevel::SessionTextOnly)`: if body contains any shell token,
///   the whole expansion is aborted with an `Err`. Otherwise it
///   proceeds without invoking the shell pass.
/// - `None`: always aborted (user cancelled the trust prompt).
pub async fn expand_all(
    body: &str,
    args: &[String],
    trust: Option<TrustLevel>,
) -> Result<Expanded, String> {
    let with_args = expand_args(body, args);
    let files = expand_files(&with_args);
    let has_shell = contains_shell_token(&files.text);
    let mut warnings = files.warnings;
    let final_text = if has_shell {
        match trust {
            Some(TrustLevel::Always) => {
                let exp = expand_shell(&files.text).await?;
                warnings.extend(exp.warnings);
                exp.text
            }
            Some(TrustLevel::SessionTextOnly) => {
                return Err(
                    "shell substitution disabled for this session (trust=session-text-only)".into(),
                );
            }
            None => return Err("dispatch aborted: user cancelled trust prompt".into()),
        }
    } else {
        match trust {
            None => return Err("dispatch aborted: user cancelled trust prompt".into()),
            _ => files.text,
        }
    };
    Ok(Expanded {
        text: final_text,
        warnings,
    })
}

async fn run_shell(cmd: &str) -> Result<String, String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await
        .map_err(|e| format!("!{cmd}: spawn failed: {e}"))?;
    if !output.status.success() {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(format!("!{cmd}: exited {code} — {stderr}"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_token() {
        assert_eq!(
            expand_args("hello $ARGUMENTS", &s(&["a", "b"])),
            "hello a b"
        );
    }

    #[test]
    fn positional() {
        assert_eq!(
            expand_args("first=$1 second=$2", &s(&["foo", "bar"])),
            "first=foo second=bar"
        );
    }

    #[test]
    fn out_of_range_is_empty() {
        assert_eq!(expand_args("[$3]", &s(&["foo"])), "[]");
    }

    #[test]
    fn no_args_is_identity_modulo_blanking_positionals() {
        assert_eq!(expand_args("plain body", &[]), "plain body");
        assert_eq!(expand_args("hi $1", &[]), "hi ");
    }

    #[test]
    fn at_path_inlines_file_contents() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        writeln!(f, "INSIDE").unwrap();
        let path = f.path().to_string_lossy().to_string();
        let body = format!("before\n@{path}\nafter");
        let exp = expand_files(&body);
        assert!(exp.text.contains("INSIDE"));
        assert!(exp.warnings.is_empty());
    }

    #[test]
    fn at_path_missing_warns_and_keeps_literal() {
        let body = "see @/no/such/file/exists.txt please";
        let exp = expand_files(body);
        assert!(exp.text.contains("@/no/such/file/exists.txt"));
        assert_eq!(exp.warnings.len(), 1);
    }

    #[test]
    fn at_in_email_like_position_is_not_expanded() {
        // `name@host` — the @ has a word-char immediately before; not a path token.
        let body = "ping me at name@host for details";
        let exp = expand_files(body);
        assert_eq!(exp.text, body);
        assert!(exp.warnings.is_empty());
    }

    #[tokio::test]
    async fn shell_substitution_inlines_stdout() {
        let exp = expand_shell("hello\n!echo from-shell\nworld")
            .await
            .unwrap();
        assert!(exp.text.contains("from-shell"));
        assert!(exp.warnings.is_empty());
    }

    #[tokio::test]
    async fn shell_substitution_nonzero_exit_is_error() {
        let err = expand_shell("!false").await.unwrap_err();
        assert!(err.contains("exit"));
    }

    #[tokio::test]
    async fn shell_substitution_inline_form() {
        let exp = expand_shell("before !`echo X` after").await.unwrap();
        assert!(exp.text.contains("X"));
    }

    #[tokio::test]
    async fn inline_backtick_does_not_split_surrounding_line() {
        // Regression for C-1: echo always emits a trailing newline.
        // The inline substitution must NOT propagate that newline so
        // "before X after" stays on one line.
        let exp = expand_shell("before !`echo X` after").await.unwrap();
        assert_eq!(exp.text, "before X after");
    }

    #[tokio::test]
    async fn unmatched_inline_backtick_emits_warning() {
        let exp = expand_shell("text !`incomplete with no close")
            .await
            .unwrap();
        assert_eq!(exp.warnings.len(), 1);
        assert!(exp.warnings[0].contains("unmatched"));
        // The original characters are still in the output (left as literal).
        assert!(exp.text.contains("!`incomplete"));
    }

    /// I-7: `!cmd` where the command binary does not exist must return an Err.
    /// On most systems `sh -c "__no_such_binary__"` spawns `sh` successfully
    /// but exits non-zero, so the error message comes from the "exited" path.
    /// We assert the error string is non-empty and contains either "spawn" or
    /// "exit" so the test tolerates both runtime paths.
    #[tokio::test]
    async fn shell_substitution_spawn_failure_is_error() {
        let result = expand_shell("!__no_such_binary_xyz_abc__").await;
        assert!(
            result.is_err(),
            "expected Err from missing binary, got: {result:?}"
        );
        let err = result.unwrap_err();
        assert!(
            !err.is_empty(),
            "error string must be non-empty, got: {err:?}"
        );
        assert!(
            err.contains("spawn") || err.contains("exit"),
            "error must mention 'spawn' or 'exit', got: {err:?}"
        );
    }

    #[tokio::test]
    async fn expand_all_runs_in_order() {
        let body = "hello $ARGUMENTS\n!echo SHELL\n@/no/such/file";
        let out = expand_all(body, &s(&["world"]), Some(TrustLevel::Always))
            .await
            .unwrap();
        assert!(out.text.contains("hello world"));
        assert!(out.text.contains("SHELL"));
        assert!(out.text.contains("@/no/such/file"));
        assert_eq!(out.warnings.len(), 1); // the @ missing-file warning
    }

    #[tokio::test]
    async fn session_text_only_skips_shell_with_error() {
        let body = "!echo X";
        let err = expand_all(body, &[], Some(TrustLevel::SessionTextOnly))
            .await
            .unwrap_err();
        assert!(err.contains("shell substitution disabled"));
    }

    #[tokio::test]
    async fn session_text_only_allows_body_without_shell() {
        let body = "Hello $1, file: @/no/such/file";
        let out = expand_all(body, &s(&["world"]), Some(TrustLevel::SessionTextOnly))
            .await
            .unwrap();
        assert!(out.text.contains("Hello world"));
    }

    #[tokio::test]
    async fn cancelled_returns_err() {
        let body = "anything";
        let err = expand_all(body, &[], None).await.unwrap_err();
        assert!(err.contains("cancelled") || err.contains("aborted"));
    }
}
