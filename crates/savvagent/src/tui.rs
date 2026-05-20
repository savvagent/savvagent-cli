use anyhow::Result;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::prelude::*;
use std::io::{self, Stdout};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> Result<Tui> {
    execute!(io::stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    // Mouse capture is best-effort: tmux/screen and a few other terminals
    // refuse it. The TUI is fully usable on the keyboard alone — wheel
    // scrolling just won't fire. Propagating the error here would strand
    // the user in the alt screen with raw mode set, since `restore` runs
    // only on the `Ok(Tui)` path.
    if let Err(err) = execute!(io::stdout(), EnableMouseCapture) {
        tracing::warn!(error = %err, "EnableMouseCapture failed; mouse-wheel scroll disabled");
    }
    // Opt into the Kitty keyboard protocol (DISAMBIGUATE_ESCAPE_CODES)
    // so terminals that support it report Shift+Enter, Ctrl+Enter, and
    // modified Esc with their modifier bits set instead of collapsing
    // to plain `Enter`/`Esc`. This is what lets `Shift+Enter` insert a
    // newline in the prompt (the `KeyCode::Enter if !SHIFT` submit guard
    // in `main.rs` then falls through to tui-textarea's `Key::Enter, ..`
    // arm). On terminals that don't recognize the escape (basic xterm,
    // macOS Terminal, gnome-terminal pre-46, …) the push is silently
    // ignored, so Shift+Enter still submits — but degrades gracefully
    // rather than failing the TUI launch.
    let _ = execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let backend = CrosstermBackend::new(io::stdout());
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

pub fn restore() -> Result<()> {
    // Pop is the symmetric pair to the push in `init`. Best-effort:
    // terminals that ignored the push will also ignore this.
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    // Same best-effort pairing for mouse capture — if `EnableMouseCapture`
    // was rejected at startup, the disable will also fail. Either way it
    // must not skip `LeaveAlternateScreen` below, or the user's terminal
    // ends a crash/quit stuck in the alt screen.
    let _ = execute!(io::stdout(), DisableMouseCapture);
    execute!(io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
