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
use ratatui_image::{picker::Picker, protocol::StatefulProtocol};
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

/// Prepare the project emblem for the terminal's graphics protocol. Must run
/// while raw mode is active but before the event loop starts reading stdin,
/// since the protocol query does a terminal roundtrip. Any failure just means
/// the help dialog renders without the emblem.
fn build_help_image() -> Option<StatefulProtocol> {
    // Composited onto black to match the help dialog's background, since the
    // emblem itself is transparent
    let image = cilki::terminal::rasterize(&cilki::emblem::CIBOX, 3.0, Some((0, 0, 0)))?;

    let picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    Some(picker.new_resize_protocol(image))
}
