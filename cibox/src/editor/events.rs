use crate::editor::state::{EditorState, Platform};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

pub fn handle_key_event(state: &mut EditorState, key: KeyEvent) {
    // Ignore release/repeat events; they would double-type in insert mode
    if key.kind != KeyEventKind::Press {
        return;
    }

    // Raw mode stops the terminal turning ^C into SIGINT, so nothing honours
    // it unless the editor does — and while a knob is being edited every
    // other key, `q` included, goes into the buffer, so there was no way out
    // at all. The reflex that gets you out of a stuck TUI, killing cibox from
    // another terminal, skips the cleanup in `EditorApp::run` and leaves the
    // shell in raw mode on the alternate screen.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        state.should_quit = true;
        return;
    }

    // crossterm reports a chord as its bare key plus a modifier, so without
    // this every binding below answers to Ctrl/Alt + its letter as well:
    // Ctrl+W wrote the pipeline files, Ctrl+D dropped a toolchain version,
    // Ctrl+P opened the platform menu, and Ctrl/Alt + any letter was typed
    // into the knob under edit and saved straight to cibox.ron. Shift is
    // deliberately absent — `J`/`K` arrive carrying it.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }

    // A text knob is being edited: capture all input
    if state.input.is_some() {
        match key.code {
            KeyCode::Enter => state.commit_input(),
            KeyCode::Esc => state.cancel_input(),
            KeyCode::Backspace => state.input_backspace(),
            KeyCode::Char(c) => state.input_push(c),
            _ => {}
        }
        return;
    }

    // If platform menu is open, handle menu navigation
    if state.platform_menu_open {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                state.close_platform_menu();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if state.platform_menu_cursor > 0 {
                    state.platform_menu_cursor -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if state.platform_menu_cursor < Platform::all().len() - 1 {
                    state.platform_menu_cursor += 1;
                }
            }
            KeyCode::Enter => {
                state.select_platform_from_menu();
            }
            _ => {}
        }
        return;
    }

    match key.code {
        // Quit
        KeyCode::Char('q') | KeyCode::Esc => {
            state.should_quit = true;
        }

        // Write CI pipeline files
        KeyCode::Char('w') | KeyCode::Char('W') => {
            state.should_write = true;
        }

        // Open platform menu
        KeyCode::Char('p') => {
            state.open_platform_menu();
        }

        // Toggle the rule/option under the cursor, or edit a text knob
        KeyCode::Enter | KeyCode::Char(' ') => {
            state.activate_current();
        }

        // Expand/collapse a rule's config options
        KeyCode::Right | KeyCode::Char('l') => {
            state.expand_current();
        }
        KeyCode::Left | KeyCode::Char('h') => {
            state.collapse_current();
        }

        // Remove the toolchain version under the cursor
        KeyCode::Char('d') | KeyCode::Delete => {
            state.delete_current_version();
        }

        // Shift-J/K scroll the preview
        KeyCode::Char('K') => {
            state.scroll_preview_up();
        }
        KeyCode::Char('J') => {
            state.scroll_preview_down();
        }

        // Up/down navigate the rule list
        KeyCode::Up | KeyCode::Char('k') => {
            state.cursor = state.cursor.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') => {
            state.cursor = (state.cursor + 1).min(state.rows.len().saturating_sub(1));
        }

        // Tab cycles the platform (alternative to the 'p' menu)
        KeyCode::Tab => {
            state.cycle_platform();
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::state::Row;
    use std::fs;
    use tempfile::tempdir;

    /// A publishable Rust project with a Dockerfile, so docker-build is
    /// detected and its `image_name` knob has a value to edit
    fn project_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
        dir
    }

    fn state_for(dir: &std::path::Path) -> EditorState {
        let facts = crate::detection::gather_facts(dir);
        EditorState::new(facts, None, dir.to_path_buf()).unwrap()
    }

    fn press(state: &mut EditorState, code: KeyCode) {
        handle_key_event(state, KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn chord(state: &mut EditorState, modifiers: KeyModifiers, c: char) {
        handle_key_event(state, KeyEvent::new(KeyCode::Char(c), modifiers));
    }

    fn rule_index(state: &EditorState, id: &str) -> usize {
        state
            .rows
            .iter()
            .position(|r| matches!(r, Row::Rule(row) if row.id == id))
            .unwrap()
    }

    /// Put the cursor on docker-build's `image_name` row and start editing it
    fn start_editing_image_name(state: &mut EditorState) {
        state.cursor = rule_index(state, "docker-build");
        press(state, KeyCode::Char('l'));
        state.cursor += 1;
        assert!(
            matches!(state.current_row(), Some(Row::TextKnob { .. })),
            "{:?}",
            state.current_row()
        );
        press(state, KeyCode::Enter);
        assert!(state.input.is_some());
    }

    #[test]
    fn test_ctrl_c_quits() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        chord(&mut state, KeyModifiers::CONTROL, 'c');
        assert!(state.should_quit);
    }

    #[test]
    fn test_ctrl_c_quits_out_of_an_edit_without_committing_it() {
        // Every other key goes into the buffer while a knob is being edited,
        // `q` included, so ^C is the only way out that doesn't need the user
        // to know Esc comes first
        let dir = project_dir();
        let mut state = state_for(dir.path());
        start_editing_image_name(&mut state);

        chord(&mut state, KeyModifiers::CONTROL, 'c');
        assert!(state.should_quit);
        assert!(!dir.path().join("cibox.ron").exists());
    }

    #[test]
    fn test_ctrl_c_quits_with_the_platform_menu_open() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        press(&mut state, KeyCode::Char('p'));
        assert!(state.platform_menu_open);

        chord(&mut state, KeyModifiers::CONTROL, 'c');
        assert!(state.should_quit);
    }

    #[test]
    fn test_ctrl_w_does_not_write_the_pipeline() {
        // ^W is "delete the last word" in readline and "close the tab" in a
        // browser; neither should put files in the user's repository
        let dir = project_dir();
        let mut state = state_for(dir.path());
        chord(&mut state, KeyModifiers::CONTROL, 'w');
        assert!(!state.should_write);

        press(&mut state, KeyCode::Char('w'));
        assert!(state.should_write);
    }

    #[test]
    fn test_ctrl_p_does_not_open_the_platform_menu() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        chord(&mut state, KeyModifiers::CONTROL, 'p');
        assert!(!state.platform_menu_open);

        press(&mut state, KeyCode::Char('p'));
        assert!(state.platform_menu_open);
    }

    #[test]
    fn test_ctrl_d_does_not_drop_a_toolchain_version() {
        let dir = project_dir();
        fs::write(
            dir.path().join("cibox.ron"),
            "(rust_test: (versions: [\"1.75\", \"stable\"]))",
        )
        .unwrap();
        let mut state = state_for(dir.path());
        state.cursor = rule_index(&state, "rust-test");
        press(&mut state, KeyCode::Char('l'));
        state.cursor += 1;
        assert!(matches!(
            state.current_row(),
            Some(Row::Version { index: Some(0), .. })
        ));

        chord(&mut state, KeyModifiers::CONTROL, 'd');
        assert_eq!(
            state.config.rust_test.versions.as_deref(),
            Some(["1.75".to_string(), "stable".to_string()].as_slice()),
        );

        press(&mut state, KeyCode::Char('d'));
        assert_eq!(
            state.config.rust_test.versions.as_deref(),
            Some(["stable".to_string()].as_slice()),
        );
    }

    #[test]
    fn test_modifier_chords_are_not_typed_into_a_knob() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        start_editing_image_name(&mut state);
        let buffer = state.input.clone().unwrap();

        chord(&mut state, KeyModifiers::CONTROL, 'x');
        chord(&mut state, KeyModifiers::ALT, 'y');
        assert_eq!(state.input.as_deref(), Some(buffer.as_str()));

        // A plain keystroke still types
        press(&mut state, KeyCode::Char('z'));
        assert_eq!(state.input.as_deref(), Some(format!("{buffer}z").as_str()));
    }

    #[test]
    fn test_shift_still_reaches_the_preview_scroll_keys() {
        // J/K arrive carrying SHIFT, so the chord filter must let it through
        let dir = project_dir();
        let mut state = state_for(dir.path());
        handle_key_event(
            &mut state,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.preview_scroll, 1);

        handle_key_event(
            &mut state,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.preview_scroll, 0);
    }
}
