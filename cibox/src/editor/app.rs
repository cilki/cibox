use crate::detection::DetectionResult;
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
use std::io;
use std::path::PathBuf;
use std::time::Duration;

pub struct EditorApp {
    state: EditorState,
}

impl EditorApp {
    pub fn new(detection: DetectionResult, platform: Option<String>) -> Result<Self> {
        let working_dir = PathBuf::from(".");

        // Check if cibox.ron exists, if so, load from it
        let cibox_ron_path = working_dir.join("cibox.ron");
        let state = if cibox_ron_path.exists() {
            EditorState::from_ron_file(&cibox_ron_path)?
        } else {
            EditorState::from_detection(detection, platform, working_dir)?
        };

        Ok(Self { state })
    }

    pub fn run(mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Run the event loop
        let result = self.event_loop(&mut terminal);

        // Cleanup
        disable_raw_mode()?;
        execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
        terminal.show_cursor()?;

        result
    }

    fn event_loop<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<()>
    where
        B::Error: Send + Sync + 'static,
    {
        loop {
            // Render
            terminal.draw(|f| render_ui(f, &self.state))?;

            // Handle events
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    handle_key_event(&mut self.state, key);
                }
            }

            // Write CI config if requested
            if self.state.should_write {
                self.write_config()?;
                self.state.should_write = false; // Reset the flag so we don't keep writing
            }

            // Check for exit
            if self.state.should_quit {
                break;
            }
        }

        Ok(())
    }

    fn write_config(&self) -> Result<()> {
        use crate::generator::MultiPresetGenerator;
        use std::fs;

        for pipeline in self.state.pipelines.iter().filter(|p| p.persist) {
            let preset_configs = self.state.enabled_preset_configs(pipeline);
            if preset_configs.is_empty() {
                continue;
            }

            let generator = MultiPresetGenerator::new(
                preset_configs,
                self.state.registry.clone(),
                pipeline.platform,
                self.state.language_version.clone(),
            );

            for (filename, content) in generator.generate_all()? {
                let output_path = self.state.working_dir.join(filename);

                if let Some(parent) = output_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                fs::write(&output_path, content)?;

                println!("✨ Generated: {}", output_path.display());
            }
        }

        Ok(())
    }
}
