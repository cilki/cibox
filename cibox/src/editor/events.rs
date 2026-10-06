use crate::editor::state::{EditorState, Platform};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

pub fn handle_key_event(state: &mut EditorState, key: KeyEvent) {
    // Ignore release/repeat events; they would double-type in insert mode
    if key.kind != KeyEventKind::Press {
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
            if state.cursor > 0 {
                state.cursor -= 1;
                state.update_current_item_description();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if state.cursor < state.rows.len().saturating_sub(1) {
                state.cursor += 1;
                state.update_current_item_description();
            }
        }

        // Tab cycles the platform (alternative to the 'p' menu)
        KeyCode::Tab => {
            state.cycle_platform();
        }

        _ => {}
    }
}
