use crate::detection::ProjectFacts;
use crate::editor::events::handle_key_event;
use crate::editor::state::EditorState;
use crate::editor::ui::render_ui;
use crate::error::Result;
use crossterm::{
    event::{self, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use ratatui_image::{
    picker::{Picker, ProtocolType},
    protocol::StatefulProtocol,
    FontSize,
};
use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub struct EditorApp {
    state: EditorState,
    /// The emblem drawn in the help dialog, encoded for the terminal's
    /// graphics protocol. Kept out of [`EditorState`] since it is only a
    /// rendering resource; `None` means the dialog renders text-only.
    help_image: Option<StatefulProtocol>,
}

impl EditorApp {
    pub fn new(facts: ProjectFacts, working_dir: PathBuf) -> Result<Self> {
        Ok(Self {
            state: EditorState::new(facts, working_dir)?,
            help_image: None,
        })
    }

    pub fn run(mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        self.help_image = build_help_image();

        // Run the event loop
        let result = self.event_loop(&mut terminal);

        // Cleanup
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result
    }

    fn event_loop<B: ratatui::backend::Backend>(&mut self, terminal: &mut Terminal<B>) -> Result<()>
    where
        B::Error: Send + Sync + 'static,
    {
        loop {
            // Render, recording the preview's size for scrolling
            let mut preview_area = None;
            terminal.draw(|f| {
                preview_area = render_ui(f, &self.state, self.help_image.as_mut());
            })?;
            self.state.set_preview_viewport(
                preview_area.map_or((0, 0), |area| (area.width, area.height)),
            );

            // Handle events
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    handle_key_event(&mut self.state, key);
                }
            }

            // Check for exit
            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }
}

/// Prepare the project emblem for the terminal's graphics protocol. Any
/// failure just means the help dialog renders without the emblem.
fn build_help_image() -> Option<StatefulProtocol> {
    // Composited onto black to match the help dialog's background, since the
    // emblem itself is transparent
    let image = cilki::terminal::rasterize(&cilki::emblem::CIBOX, 3.0, Some((0, 0, 0)))?;
    Some(help_picker().new_resize_protocol(image))
}

/// The protocol to draw the emblem with, decided from the environment rather
/// than by asking the terminal.
///
/// The usual way to find out is ratatui-image's `Picker::from_query_stdio`,
/// which writes a capability query to stdout and reads the answer off stdin.
/// A terminal that does not recognize the query simply never answers, and
/// then two things happen: the editor shows nothing for the two seconds the
/// query waits, and — because the thread it reads stdin on outlives that
/// timeout and is still blocked in `read` — the keystroke that finally
/// unblocks it is consumed there instead of reaching the event loop. The
/// user's first keypress is silently swallowed, `q` included.
///
/// So nothing here writes to the terminal or reads from it; this is the same
/// trade `cilki::terminal` makes for the emblem it prints above `--help`, and
/// the same reason. A terminal that implements a protocol without saying so
/// in the environment gets half-blocks, which the dialog renders perfectly
/// well — it is a picture in a help dialog, not something worth stalling
/// every start for.
fn help_picker() -> Picker {
    let var = |name: &str| std::env::var(name).ok();

    if in_multiplexer(var) {
        return Picker::halfblocks();
    }
    // The only constructor that takes a real cell size; the alternatives
    // hardcode one. It detects the iTerm2 family from the environment itself,
    // but declines to guess kitty, whose marker variables go stale inside a
    // multiplexer — the case just ruled out, which is what makes the guess
    // safe to make here.
    #[allow(deprecated)]
    let mut picker = Picker::from_fontsize(cell_size());
    if kitty_from_env(var) {
        picker.set_protocol_type(ProtocolType::Kitty);
    }
    picker
}

/// Whether a terminal multiplexer is in the way.
///
/// A multiplexer redraws everything it is handed, so a graphics escape sent
/// through one is dropped or mangled unless it is wrapped for a passthrough
/// that only a query could confirm is enabled.
fn in_multiplexer(var: impl Fn(&str) -> Option<String>) -> bool {
    let term = var("TERM").unwrap_or_default();
    term.starts_with("tmux")
        || term.starts_with("screen")
        || var("TERM_PROGRAM").is_some_and(|program| program == "tmux")
        || ["TMUX", "STY", "ZELLIJ"]
            .iter()
            .any(|name| var(name).is_some_and(|value| !value.is_empty()))
}

/// Whether this is a terminal that draws kitty's graphics protocol.
fn kitty_from_env(var: impl Fn(&str) -> Option<String>) -> bool {
    let nonempty = |name: &str| var(name).is_some_and(|value| !value.is_empty());
    let term = var("TERM").unwrap_or_default();

    term == "xterm-kitty"
        || term == "xterm-ghostty"
        || nonempty("KITTY_WINDOW_ID")
        || nonempty("GHOSTTY_RESOURCES_DIR")
        || var("TERM_PROGRAM").is_some_and(|program| program == "ghostty")
}

/// The terminal's cell size in pixels, from the window-size ioctl rather than
/// a query.
///
/// A terminal that reports no pixel geometry — kitty over a plain `ssh`, for
/// one — still draws the image perfectly well at a guessed cell size, just at
/// a slightly wrong aspect, which beats no image.
fn cell_size() -> FontSize {
    const GUESS: FontSize = FontSize {
        width: 10,
        height: 20,
    };
    let Ok(size) = crossterm::terminal::window_size() else {
        return GUESS;
    };
    if size.columns == 0 || size.rows == 0 || size.width == 0 || size.height == 0 {
        return GUESS;
    }
    let (width, height) = (size.width / size.columns, size.height / size.rows);
    // A cell outside these bounds means the report is nonsense, not a font
    if (4..=40).contains(&width) && (8..=80).contains(&height) {
        FontSize::new(width, height)
    } else {
        GUESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Look variables up in a fixed table, so the decision can be tested
    /// without touching the process environment
    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn test_kitty_family_detected_without_a_query() {
        for pairs in [
            [("TERM", "xterm-kitty")].as_slice(),
            [("TERM", "xterm-ghostty")].as_slice(),
            [("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "1")].as_slice(),
            [
                ("TERM", "xterm-256color"),
                ("GHOSTTY_RESOURCES_DIR", "/usr/share/ghostty"),
            ]
            .as_slice(),
            [("TERM", "xterm-256color"), ("TERM_PROGRAM", "ghostty")].as_slice(),
        ] {
            assert!(kitty_from_env(env(pairs)), "{pairs:?}");
        }
    }

    #[test]
    fn test_plain_terminal_is_not_taken_for_kitty() {
        for pairs in [
            [].as_slice(),
            [("TERM", "xterm-256color")].as_slice(),
            // An empty marker is not a marker
            [("TERM", "xterm-256color"), ("KITTY_WINDOW_ID", "")].as_slice(),
            [("TERM", "xterm-256color"), ("GHOSTTY_RESOURCES_DIR", "")].as_slice(),
        ] {
            assert!(!kitty_from_env(env(pairs)), "{pairs:?}");
            assert!(!in_multiplexer(env(pairs)), "{pairs:?}");
        }
    }

    #[test]
    fn test_multiplexer_detected_from_any_of_its_markers() {
        for pairs in [
            [("TERM", "tmux-256color")].as_slice(),
            [("TERM", "screen.xterm-256color")].as_slice(),
            [("TERM", "xterm-256color"), ("TERM_PROGRAM", "tmux")].as_slice(),
            [("TERM", "xterm-256color"), ("TMUX", "/tmp/tmux-0/default")].as_slice(),
            [("TERM", "xterm-256color"), ("STY", "1234.pts-0")].as_slice(),
            [("TERM", "xterm-256color"), ("ZELLIJ", "0")].as_slice(),
        ] {
            assert!(in_multiplexer(env(pairs)), "{pairs:?}");
        }
    }

    /// A kitty marker inside a multiplexer is not trusted: the pane may have
    /// been detached from kitty and reattached somewhere that draws nothing,
    /// and a graphics escape would not survive the multiplexer regardless
    #[test]
    fn test_stale_kitty_marker_inside_a_multiplexer_is_ignored() {
        let pairs = [("TERM", "tmux-256color"), ("KITTY_WINDOW_ID", "1")];
        assert!(kitty_from_env(env(&pairs)));
        assert!(in_multiplexer(env(&pairs)));
    }

    #[test]
    fn test_cell_size_is_a_plausible_cell() {
        let size = cell_size();
        assert!((4..=40).contains(&size.width), "{size:?}");
        assert!((8..=80).contains(&size.height), "{size:?}");
    }
}
