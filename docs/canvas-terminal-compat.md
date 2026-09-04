# Inline HTML canvas — terminal compatibility

The canvas feature places rendered HTML in the conversation transcript
via your terminal's image protocol. Detection runs once at startup
using `ratatui-image`'s `Picker::from_query_stdio`; terminals without
a supported protocol fall back to a syntax-highlighted source-code view
with a one-line banner.

## Supported

| Terminal | Protocol | Notes |
|---|---|---|
| Kitty | kitty graphics | First-class. Incremental frame updates. |
| Ghostty | kitty graphics | First-class. |
| WezTerm | iTerm2 inline | First-class. |
| iTerm2 (macOS) | iTerm2 inline | First-class. |
| Any sixel-capable terminal | sixel | Works. Full-frame updates; colour depth varies by terminal. |

Sixel support is implemented through the `crossterm` backend's built-in
sixel encoder — savvagent does **not** depend on the `chafa` command-line
tool. Any terminal that advertises sixel capability in its `XTSMGRAPHICS`
response (including Alacritty with sixel patches, mlterm, foot, and
xterm compiled with `--enable-sixel-graphics`) should work.

## Tmux

Tmux intercepts terminal escape sequences by default, which breaks image
protocols. Enable passthrough to let the sequences reach the outer
terminal:

```bash
# Add to ~/.tmux.conf
set -g allow-passthrough on
```

Reload the config (`tmux source ~/.tmux.conf`) or restart tmux for the
change to take effect. If you see corrupted output or no images inside
tmux, this is the most likely cause.

> **Note:** `allow-passthrough` requires tmux 3.3 or later. Check with
> `tmux -V`.

## Unsupported terminals (source-code fallback)

Terminals that report no image protocol — plain xterm without sixel,
most virtual consoles, `screen` — fall back to the source-code view.
The HTML source is shown in a syntax-highlighted code block with a
yellow banner:

```
Inline HTML rendering requires kitty / WezTerm / Ghostty / iTerm2 / Sixel.
```

Functionality is fully preserved: the model's structured content is
readable, and auto-export to `~/.savvagent/canvases/` still runs so
you can open the file in a browser.

## SSH

Image protocols work over SSH when the local terminal supports them.
The standard `ssh` client forwards escape sequences without special
configuration. No additional setup is needed.

If using `mosh`, note that mosh does not forward arbitrary escape
sequences; image protocols will not work inside a mosh session.

## Troubleshooting

**Source-code fallback appears but you expect images.**
Your terminal's image protocol was not detected at startup. Check that
your terminal is in the supported list above. For sixel, verify sixel
support is compiled in (e.g. `xterm -v` includes "sixel"; Alacritty
requires an unofficial patch). Re-check with a fresh terminal window —
the protocol query runs once on attach.

**Images render glitched or torn inside tmux.**
Passthrough is likely disabled. Add `set -g allow-passthrough on` to
`~/.tmux.conf` and reload. See the Tmux section above.

**Images render glitched or torn outside tmux.**
File a bug at <https://github.com/savvagent/savvagent-cli/issues> with
your terminal name and version.

**Render is blocky or wrong colours in a sixel terminal.**
Sixel colour depth is terminal-specific. For the best visual fidelity,
use a Kitty or iTerm2-protocol terminal. For sixel terminals, check
whether your terminal supports 256 colours or more.

**Auto-exported canvases are not appearing.**
Confirm the `internal:html-canvas` plugin is enabled (`/plugins`). The
export directory is `~/.savvagent/canvases/`; it is created on first
export. Check `~/.savvagent/logs/savvagent.log` for any I/O errors.
