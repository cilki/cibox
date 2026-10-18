use crate::editor::state::{EditorState, Focus};
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
    // Ctrl+D dropped a toolchain version, Ctrl+Q quit, and Ctrl/Alt + any
    // letter was typed into the knob under edit and saved straight to
    // cibox.ron. Shift is deliberately absent — `?` arrives carrying it.
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return;
    }

    // The help dialog swallows all input except '?'/Esc to close it, so a
    // stray key can't act on the rules hidden behind it
    if state.help_open {
        if matches!(key.code, KeyCode::Char('?') | KeyCode::Esc) {
            state.help_open = false;
        }
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

    // Keys every panel answers to
    match key.code {
        KeyCode::Char('q') => {
            state.should_quit = true;
            return;
        }
        KeyCode::Char('?') => {
            state.help_open = true;
            return;
        }
        KeyCode::Tab => {
            state.toggle_focus();
            return;
        }
        _ => {}
    }

    match state.focus {
        Focus::Rules => match key.code {
            KeyCode::Esc => state.should_quit = true,

            // Toggle the rule/option under the cursor, or edit a text knob
            KeyCode::Enter | KeyCode::Char(' ') => state.activate_current(),

            // Expand/collapse a rule's config options
            KeyCode::Right => state.expand_current(),
            KeyCode::Left => state.collapse_current(),

            // Remove the toolchain version under the cursor
            KeyCode::Char('d') | KeyCode::Delete => state.delete_current_version(),

            // The preview follows the cursor to its rule's job
            KeyCode::Up => state.move_cursor(-1),
            KeyCode::Down => state.move_cursor(1),
            _ => {}
        },
        Focus::Preview => match key.code {
            // Back out of the preview rather than out of the editor
            KeyCode::Esc => state.focus = Focus::Rules,
            KeyCode::Up => state.scroll_preview(-1),
            KeyCode::Down => state.scroll_preview(1),
            KeyCode::PageUp => state.scroll_preview(-state.preview_page()),
            KeyCode::PageDown => state.scroll_preview(state.preview_page()),
            _ => {}
        },
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

    const CI_YML: &str = ".github/workflows/ci.yml";

    /// [`project_dir`] with a stale GitHub pipeline on disk, so there is a
    /// pending update for the preview to show
    fn project_with_pipeline() -> tempfile::TempDir {
        let dir = project_dir();
        fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        fs::write(
            dir.path().join(CI_YML),
            "on: push\njobs:\n  rust-test:\n    runs-on: ubuntu-latest\n    steps:\n      \
             - run: echo stale\n",
        )
        .unwrap();
        dir
    }

    fn state_for(dir: &std::path::Path) -> EditorState {
        let facts = crate::detection::gather_facts(dir);
        EditorState::new(facts, dir.to_path_buf()).unwrap()
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
        press(state, KeyCode::Right);
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
    fn test_editor_never_writes_the_pipeline() {
        // Pipelines are only written by `cibox update`; the keys that used
        // to write them, or pick the platform to write, do nothing
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        let before = fs::read_to_string(dir.path().join(CI_YML)).unwrap();
        let platforms = state.platforms.clone();

        for c in ['w', 'W', 'p'] {
            press(&mut state, KeyCode::Char(c));
        }
        assert_eq!(fs::read_to_string(dir.path().join(CI_YML)).unwrap(), before);
        assert!(!dir.path().join(".github/workflows/release.yml").exists());
        assert_eq!(state.platforms, platforms);
        assert!(!state.should_quit);
    }

    #[test]
    fn test_vim_keys_are_unbound() {
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        state.cursor = 1;
        let rows = state.rows.len();
        let scroll = state.preview_scroll;

        for c in ['h', 'j', 'k', 'l', 'J', 'K'] {
            press(&mut state, KeyCode::Char(c));
        }
        assert_eq!(state.cursor, 1);
        assert_eq!(state.rows.len(), rows, "nothing was expanded");
        assert_eq!(state.preview_scroll, scroll);
    }

    #[test]
    fn test_tab_moves_focus_between_rules_and_preview() {
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        assert!(state.preview_shown());
        assert_eq!(state.focus, Focus::Rules);

        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Preview);
        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Rules);
    }

    #[test]
    fn test_tab_does_not_focus_a_hidden_preview() {
        // The pipeline on disk is already what update would write, so there
        // is nothing to preview
        let dir = project_dir();
        let mut state = state_for(dir.path());
        let resolved = crate::rules::resolve(&state.facts, &state.config);
        for (path, content) in
            crate::generator::generate(&state.facts, &resolved, state.platforms[0]).unwrap()
        {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
        state.refresh();
        assert!(!state.preview_shown());

        press(&mut state, KeyCode::Tab);
        assert_eq!(state.focus, Focus::Rules);
    }

    #[test]
    fn test_esc_in_the_preview_returns_to_the_rules() {
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        press(&mut state, KeyCode::Tab);

        press(&mut state, KeyCode::Esc);
        assert_eq!(state.focus, Focus::Rules);
        assert!(!state.should_quit);

        // From the rules, Esc still quits
        press(&mut state, KeyCode::Esc);
        assert!(state.should_quit);
    }

    #[test]
    fn test_arrows_scroll_the_focused_preview() {
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        state.preview_viewport = (80, 5);
        // From the top, rather than wherever the cursor's rule put it
        state.preview_scroll = 0;
        let cursor = state.cursor;
        press(&mut state, KeyCode::Tab);

        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.preview_scroll, 2);
        assert_eq!(state.cursor, cursor, "the rules cursor stays put");
        press(&mut state, KeyCode::Up);
        assert_eq!(state.preview_scroll, 1);

        press(&mut state, KeyCode::PageDown);
        assert_eq!(state.preview_scroll, 5);
        press(&mut state, KeyCode::PageUp);
        press(&mut state, KeyCode::PageUp);
        assert_eq!(state.preview_scroll, 0, "never above the top");

        // Never past the end either: paging down stops somewhere
        let mut max = state.preview_scroll;
        loop {
            press(&mut state, KeyCode::PageDown);
            if state.preview_scroll == max {
                break;
            }
            max = state.preview_scroll;
        }
        assert!(max > 5);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.preview_scroll, max);
    }

    #[test]
    fn test_arrows_in_the_rules_take_the_preview_to_the_rule_job() {
        let dir = project_with_pipeline();
        let mut state = state_for(dir.path());
        state.preview_viewport = (80, 5);
        let target = rule_index(&state, "docker-build");
        state.cursor = target - 1;
        state.preview_scroll = 0;

        press(&mut state, KeyCode::Down);
        assert_eq!(state.cursor, target);
        let (width, height) = state.preview_viewport;
        let anchor = state.preview.anchor("docker-build").expect("job is added");
        let expected = state
            .preview
            .rows_before(anchor, width)
            .min(state.preview.max_scroll(width, height));
        assert!(expected > 0, "docker-build's job starts below the top");
        assert_eq!(state.preview_scroll, expected);

        // Scrolling the focused preview by hand is not undone...
        press(&mut state, KeyCode::Tab);
        press(&mut state, KeyCode::Up);
        assert_eq!(state.preview_scroll, expected - 1);
        // ...until the rules cursor moves again
        press(&mut state, KeyCode::Tab);
        press(&mut state, KeyCode::Up);
        press(&mut state, KeyCode::Down);
        assert_eq!(state.preview_scroll, expected);
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
        press(&mut state, KeyCode::Right);
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
    fn test_question_mark_toggles_help() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        // Terminals report '?' carrying SHIFT
        handle_key_event(
            &mut state,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
        );
        assert!(state.help_open);

        press(&mut state, KeyCode::Char('?'));
        assert!(!state.help_open);
    }

    #[test]
    fn test_esc_closes_help_without_quitting() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        press(&mut state, KeyCode::Char('?'));

        press(&mut state, KeyCode::Esc);
        assert!(!state.help_open);
        assert!(!state.should_quit);
    }

    #[test]
    fn test_help_swallows_other_keys() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        press(&mut state, KeyCode::Char('?'));
        let cursor = state.cursor;

        press(&mut state, KeyCode::Char('q'));
        press(&mut state, KeyCode::Down);
        press(&mut state, KeyCode::Tab);
        assert!(state.help_open);
        assert!(!state.should_quit);
        assert_eq!(state.focus, Focus::Rules);
        assert_eq!(state.cursor, cursor);
    }

    #[test]
    fn test_ctrl_c_quits_with_help_open() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        press(&mut state, KeyCode::Char('?'));

        chord(&mut state, KeyModifiers::CONTROL, 'c');
        assert!(state.should_quit);
    }

    #[test]
    fn test_question_mark_is_typed_into_a_knob() {
        let dir = project_dir();
        let mut state = state_for(dir.path());
        start_editing_image_name(&mut state);
        let buffer = state.input.clone().unwrap();

        press(&mut state, KeyCode::Char('?'));
        assert!(!state.help_open);
        assert_eq!(state.input.as_deref(), Some(format!("{buffer}?").as_str()));
    }
}
