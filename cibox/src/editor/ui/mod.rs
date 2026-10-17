use crate::editor::state::{EditorState, Focus, Row, RuleRow};
use crate::generator::{FileChange, FileUpdate};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use ratatui_image::{protocol::StatefulProtocol, StatefulImage};
use similar::{ChangeTag, TextDiff};

/// Lines of context kept around each change, as in `diff -U3`
const DIFF_CONTEXT: usize = 3;

/// What one line of the preview is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffType {
    /// Unchanged context around a change
    Unchanged,
    Added,
    Removed,
    /// `# ==> path <==` naming the file the hunks below it belong to
    File,
    /// `@@ -a,b +c,d @@` opening a hunk
    Hunk,
    /// Why `cibox update` would fail, in place of a diff
    Error,
}

/// Background of an added line. A dark tint rather than ANSI green: the line
/// keeps its YAML highlighting (cyan keys, green strings, yellow numbers),
/// and those foregrounds wash out against a full-intensity background.
const ADDED_BG: Color = Color::Rgb(0x12, 0x3d, 0x1f);
/// Background of a removed line; a dark tint for the same reason
const REMOVED_BG: Color = Color::Rgb(0x4b, 0x16, 0x1a);

impl DiffType {
    fn background(self) -> Option<Color> {
        match self {
            DiffType::Added => Some(ADDED_BG),
            DiffType::Removed => Some(REMOVED_BG),
            _ => None,
        }
    }
}

/// What the preview panel draws: the changes the next `cibox update` would
/// make to the pipeline files, as unified-diff hunks per file.
///
/// Built once per change of the preview rather than once per frame. The
/// editor redraws on every turn of its event loop — ten times a second with
/// nobody touching the keyboard — and diffing a large existing pipeline is
/// not free: 3.8ms for ten thousand lines, plus the cost of styling every
/// one of them, almost all of it off screen.
#[derive(Debug, Default)]
pub struct PreviewDiff {
    lines: Vec<(String, DiffType)>,
}

impl PreviewDiff {
    /// The diff of everything `updates` would write against what is on disk.
    /// Files update would create are shown whole, as added lines — in a
    /// repository without CI config, that is the whole pipeline. Empty, so
    /// the panel is hidden, only when update would change nothing.
    pub fn from_updates(updates: &[FileUpdate]) -> Self {
        let mut preview = Self::default();
        for update in updates {
            if let FileChange::Write { content, .. } = &update.change {
                preview.push_file(update, content);
            }
        }
        preview
    }

    /// A preview that only says why it can't show a diff
    pub fn error(message: &str) -> Self {
        Self {
            lines: message
                .lines()
                .map(|line| (line.to_string(), DiffType::Error))
                .collect(),
        }
    }

    fn push_file(&mut self, update: &FileUpdate, new: &str) {
        let old = update.existing.as_deref().unwrap_or("");
        let diff = TextDiff::from_lines(old, new);
        let mut hunks = diff.unified_diff();
        let mut hunks = hunks.context_radius(DIFF_CONTEXT).iter_hunks().peekable();
        if hunks.peek().is_none() {
            return;
        }

        if !self.lines.is_empty() {
            self.lines.push((String::new(), DiffType::Unchanged));
        }
        let new_file = if update.existing.is_none() {
            " (new)"
        } else {
            ""
        };
        self.lines.push((
            format!("# ==> {}{new_file} <==", update.path.display()),
            DiffType::File,
        ));
        for hunk in hunks {
            self.lines.push((hunk.header().to_string(), DiffType::Hunk));
            for change in hunk.iter_changes() {
                let kind = match change.tag() {
                    ChangeTag::Equal => DiffType::Unchanged,
                    ChangeTag::Delete => DiffType::Removed,
                    ChangeTag::Insert => DiffType::Added,
                };
                let text = change.value().trim_end_matches(['\n', '\r']).to_string();
                self.lines.push((text, kind));
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The furthest the preview can scroll in a panel of the given inner
    /// size: until its last wrapped row sits at the bottom
    pub fn max_scroll(&self, width: u16, height: u16) -> u16 {
        if width == 0 {
            return 0;
        }
        let rows = Paragraph::new(self.visible(0, u16::MAX))
            .wrap(Wrap { trim: false })
            .line_count(width);
        u16::try_from(rows.saturating_sub(usize::from(height))).unwrap_or(u16::MAX)
    }

    /// Styled lines for a panel `rows` rows tall scrolled down by `scroll`,
    /// YAML highlighted over the background color of its diff status.
    ///
    /// Wrapping gives every line at least one row, so the last visible row —
    /// `scroll + rows` — can only be fed by a line at that index or earlier;
    /// styling anything beyond it is work the panel throws away. The lines
    /// above `scroll` do have to be handed over: the offset counts wrapped
    /// rows, and only the paragraph knows how many rows each line takes.
    fn visible(&self, scroll: u16, rows: u16) -> Vec<Line<'static>> {
        let end = usize::from(scroll).saturating_add(usize::from(rows));
        self.lines[..self.lines.len().min(end)]
            .iter()
            .map(|(line, diff)| match diff {
                DiffType::File => Line::from(Span::styled(
                    line.clone(),
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                DiffType::Hunk => {
                    Line::from(Span::styled(line.clone(), Style::default().fg(Color::Blue)))
                }
                DiffType::Error => {
                    Line::from(Span::styled(line.clone(), Style::default().fg(Color::Red)))
                }
                _ => highlight_line(line, diff.background()),
            })
            .collect()
    }
}

/// Draw the editor. `help_image` is the emblem shown in the help dialog,
/// already encoded for the terminal; `None` draws the dialog text-only.
///
/// Returns the inner area of the preview panel, `None` when it is hidden,
/// which is what preview scrolling is measured against.
pub fn render_ui(
    f: &mut Frame,
    state: &EditorState,
    help_image: Option<&mut StatefulProtocol>,
) -> Option<Rect> {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Information message bar
            Constraint::Min(0),    // Main content
        ])
        .split(f.area());

    render_info_bar(f, chunks[0], state);

    // Rules on the left, and the pending update on the right when there is
    // one; otherwise the rules take the whole width
    let preview_area = if state.preview_shown() {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);
        render_rules_panel(f, main_chunks[0], state);
        Some(render_preview_panel(f, main_chunks[1], state))
    } else {
        render_rules_panel(f, chunks[1], state);
        None
    };

    // Keybindings overlay, above everything else
    if state.help_open {
        render_help_dialog(f, help_image);
    }

    preview_area
}

/// The border style of a panel: highlighted in `color` while it has focus
fn panel_border(focused: bool, color: Color) -> Style {
    Style::default().fg(if focused { color } else { Color::DarkGray })
}

fn render_info_bar(f: &mut Frame, area: Rect, state: &EditorState) {
    let description = state.current_description();
    let text = if description.is_empty() {
        "Navigate with ↑↓, toggle rules with Space/Enter".to_string()
    } else {
        description
    };

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Information "),
        );

    f.render_widget(paragraph, area);
}

fn render_rules_panel(f: &mut Frame, area: Rect, state: &EditorState) {
    let mut items: Vec<ListItem> = Vec::new();

    for (i, row) in state.rows.iter().enumerate() {
        let is_selected = i == state.cursor;

        let line = match (row, state.editing(i)) {
            // The knob under edit: its own prefix, then the buffer and caret
            (Row::TextKnob { .. } | Row::Version { .. }, Some(buffer)) => Line::from(vec![
                dim(knob_prefix(row)),
                Span::styled(format!("{buffer}▏"), Style::default().fg(Color::Yellow)),
            ]),
            (Row::Rule(rule), _) => rule_line(rule, is_selected),
            (
                Row::TextKnob {
                    override_value,
                    effective,
                    ..
                },
                _,
            ) => {
                let overridden = override_value.is_some();
                let marker = if overridden { " *" } else { "" };
                Line::from(vec![
                    dim(knob_prefix(row)),
                    Span::styled(
                        format!("{effective}{marker}"),
                        Style::default().fg(knob_value_color(is_selected, overridden)),
                    ),
                ])
            }
            (Row::Checkbox { knob, value }, _) => Line::from(vec![
                Span::styled(
                    format!("       {} ", if *value { "[✓]" } else { "[ ]" }),
                    Style::default().fg(if *value { Color::Green } else { Color::DarkGray }),
                ),
                Span::styled(
                    knob.label(),
                    Style::default().fg(knob_value_color(is_selected, *value)),
                ),
            ]),
            (Row::Version { index: Some(_), value, .. }, _) => Line::from(vec![
                dim(knob_prefix(row)),
                Span::styled(
                    value.clone(),
                    Style::default().fg(knob_value_color(is_selected, true)),
                ),
            ]),
            // The add row holds no value of its own, so it stays dimmed
            (Row::Version { index: None, .. }, _) => Line::from(Span::styled(
                "       [+] add version…",
                Style::default().fg(knob_value_color(is_selected, false)),
            )),
        };

        let item_style = if is_selected {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        items.push(ListItem::new(line).style(item_style));
    }

    // While a knob is being edited every key goes into the buffer, so the
    // title says how to get out of it
    let title = if state.input.is_some() {
        Line::from(vec![
            Span::raw(" Editing — "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" save · "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" cancel "),
        ])
    } else {
        Line::from(" Rules (* = overridden in cibox.ron) ")
    };

    let list = List::new(items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(panel_border(state.focus == Focus::Rules, Color::Green)),
    );

    f.render_widget(list, area);
}

/// The dim label a knob row draws before its value, which doubles as the
/// prefix shown while that knob is being edited. Empty for the rows that
/// hold no editable value and so never ask for one.
fn knob_prefix(row: &Row) -> String {
    match row {
        Row::TextKnob { knob, .. } => format!("       {}: ", knob.label()),
        Row::Version { index: Some(_), .. } => "       - ".to_string(),
        Row::Version { index: None, .. } => "       + ".to_string(),
        Row::Rule(_) | Row::Checkbox { .. } => String::new(),
    }
}

fn dim(text: String) -> Span<'static> {
    Span::styled(text, Style::default().fg(Color::DarkGray))
}

/// Color of a knob's value: highlighted under the cursor, dimmed while it
/// holds its default
fn knob_value_color(is_selected: bool, set: bool) -> Color {
    match (is_selected, set) {
        (true, _) => Color::Yellow,
        (false, true) => Color::White,
        (false, false) => Color::DarkGray,
    }
}

fn rule_line(row: &RuleRow, is_selected: bool) -> Line<'static> {
    // Detected-but-disabled means cibox.ron explicitly disables the rule
    let checkbox = match (row.enabled, row.detected) {
        (true, _) => "[✓]",
        (false, true) => "[-]",
        (false, false) => "[ ]",
    };
    let checkbox_color = match (row.enabled, row.detected) {
        (true, true) => Color::Green,
        (true, false) | (false, true) => Color::Yellow,
        (false, false) => Color::DarkGray,
    };

    let text_color = if is_selected {
        Color::Yellow
    } else if row.enabled {
        Color::White
    } else {
        Color::DarkGray
    };

    // Expand indicator for rules with config options
    let arrow = match (row.expandable, row.expanded) {
        (true, true) => "▾ ",
        (true, false) => "▸ ",
        (false, _) => "  ",
    };

    Line::from(vec![
        Span::styled(format!(" {checkbox} "), Style::default().fg(checkbox_color)),
        Span::styled(arrow.to_string(), Style::default().fg(Color::DarkGray)),
        Span::styled(row.id.to_string(), Style::default().fg(text_color)),
        Span::styled(
            format!("  {}", row.name),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// Draw the pending `cibox update` diff, returning the panel's inner area
fn render_preview_panel(f: &mut Frame, area: Rect, state: &EditorState) -> Rect {
    let block = Block::default()
        .title(format!(
            " Pending `cibox update` ({}) ",
            crate::config::platform_names(&state.platforms)
        ))
        .borders(Borders::ALL)
        .border_style(panel_border(state.focus == Focus::Preview, Color::Cyan));
    let inner = block.inner(area);

    let preview = Paragraph::new(state.preview.visible(state.preview_scroll, inner.height))
        .wrap(Wrap { trim: false })
        .scroll((state.preview_scroll, 0))
        .block(block);
    f.render_widget(preview, area);

    inner
}

/// Syntax-highlight one YAML line over an optional diff background color
fn highlight_line(line: &str, bg: Option<Color>) -> Line<'static> {
    let body = line.trim_start();
    if body.is_empty() {
        return Line::from("");
    }

    let span = |text: String, fg: Option<Color>| {
        let mut style = Style::default();
        if let Some(fg) = fg {
            style = style.fg(fg);
        }
        if let Some(bg) = bg {
            style = style.bg(bg);
        }
        Span::styled(text, style)
    };

    if body.starts_with('#') {
        return Line::from(span(line.to_string(), Some(Color::DarkGray)));
    }

    let mut spans = Vec::new();
    let indent = line.len() - body.len();
    if indent > 0 {
        spans.push(span(" ".repeat(indent), None));
    }

    if let Some((key, rest)) = body.split_once(':') {
        spans.push(span(key.to_string(), Some(Color::Cyan)));
        spans.push(span(":".to_string(), None));
        let value = rest.trim_start();
        if !value.is_empty() {
            spans.push(span(" ".to_string(), None));
            spans.push(span(value.to_string(), value_color(value)));
        }
    } else if let Some(rest) = body.strip_prefix("- ") {
        spans.push(span("- ".to_string(), Some(Color::Yellow)));
        spans.push(span(rest.to_string(), None));
    } else {
        spans.push(span(body.to_string(), None));
    }

    Line::from(spans)
}

/// Color for a scalar value: quoted strings, booleans and numbers stand out
fn value_color(value: &str) -> Option<Color> {
    if value.starts_with('"') || value.starts_with('\'') {
        Some(Color::Green)
    } else if value == "true" || value == "false" {
        Some(Color::Magenta)
    } else if value.parse::<f64>().is_ok() {
        Some(Color::Yellow)
    } else {
        None
    }
}

/// The keybindings listed in the help dialog: everything normal mode answers
/// to (see `events::handle_key_event`)
const HELP_BINDINGS: &[(&str, Color, &str)] = &[
    ("↑↓", Color::Blue, "navigate / scroll"),
    ("PgUp/PgDn", Color::Blue, "scroll a page"),
    ("Space/Enter", Color::Yellow, "toggle/edit"),
    ("←→", Color::Yellow, "expand/collapse"),
    ("d/Del", Color::Red, "remove version"),
    ("Tab", Color::Cyan, "switch rules/preview"),
    ("?", Color::Cyan, "help"),
    ("q/Esc", Color::Red, "quit"),
];

/// Rows the emblem gets at the top of the help dialog
const HELP_IMAGE_ROWS: u16 = 7;

/// Render the keybindings dialog toggled by '?', with the cibox emblem above
/// the bindings when the terminal could draw it
fn render_help_dialog(f: &mut Frame, image: Option<&mut StatefulProtocol>) {
    let image_rows = if image.is_some() { HELP_IMAGE_ROWS } else { 0 };

    let area = f.area();
    let dialog_width = area.width.min(56);
    let dialog_height = area.height.min(image_rows + HELP_BINDINGS.len() as u16 + 2);
    let dialog_area = Rect::new(
        area.x + (area.width.saturating_sub(dialog_width)) / 2,
        area.y + (area.height.saturating_sub(dialog_height)) / 2,
        dialog_width,
        dialog_height,
    );

    f.render_widget(Clear, dialog_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(dialog_area);
    f.render_widget(block, dialog_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(image_rows), Constraint::Min(0)])
        .split(inner);

    if let Some(protocol) = image {
        f.render_stateful_widget(StatefulImage::default(), chunks[0], protocol);
    }

    // Two aligned columns, centered as a block (centering each line
    // individually would break the key column)
    let key_width = HELP_BINDINGS
        .iter()
        .map(|(key, _, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    let block_width = key_width
        + 2
        + HELP_BINDINGS
            .iter()
            .map(|(_, _, description)| description.chars().count())
            .max()
            .unwrap_or(0);
    let indent = " ".repeat((chunks[1].width as usize).saturating_sub(block_width) / 2);
    let lines: Vec<Line> = HELP_BINDINGS
        .iter()
        .map(|(key, color, description)| {
            let pad = " ".repeat(key_width - key.chars().count());
            Line::from(vec![
                Span::raw(format!("{indent}{pad}")),
                Span::styled(
                    *key,
                    Style::default().fg(*color).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(*description),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), chunks[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Editor state for a Rust project with a Dockerfile, with every
    /// expandable rule open — the only state in which knob rows get drawn
    fn expanded_state() -> (tempfile::TempDir, EditorState) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
        let facts = crate::detection::gather_facts(dir.path());
        let mut state = EditorState::new(facts, dir.path().to_path_buf()).unwrap();
        for id in ["rust-test", "docker-release"] {
            state.cursor = state
                .rows
                .iter()
                .position(|row| matches!(row, Row::Rule(rule) if rule.id == id))
                .unwrap();
            state.expand_current();
        }
        (dir, state)
    }

    /// The rules panel drawn tall enough to hold every row, one String per
    /// terminal line
    fn rendered_panel(state: &EditorState) -> Vec<String> {
        let (width, height) = (64u16, state.rows.len() as u16 + 2);
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render_rules_panel(f, Rect::new(0, 0, width, height), state))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// Every config option of an expanded rule is one row of the same shape,
    /// whichever knob it holds
    #[test]
    fn test_knob_rows_render_at_a_common_indent() {
        let (_dir, state) = expanded_state();
        let lines = rendered_panel(&state);
        let row = |needle: &str| {
            let line = lines
                .iter()
                .find(|line| line.contains(needle))
                .unwrap_or_else(|| panic!("no row for {needle}:\n{}", lines.join("\n")));
            // Drop the panel's left border
            line.trim_start_matches('│').to_string()
        };

        for knob in ["image_name:", "sync_readme", "linux/amd64", "add version…"] {
            let row = row(knob);
            assert!(row.starts_with("       "), "{knob}: {row:?}");
            assert!(!row.starts_with("        "), "{knob}: {row:?}");
        }
        // The two boolean knobs are checkboxes, the rest are not
        assert!(row("sync_readme").contains("[ ]"));
        assert!(row("linux/amd64").contains("[ ]"));
        assert!(!row("image_name:").contains("[ ]"));
    }

    /// An edit in progress is drawn on the row it belongs to — the one under
    /// the cursor — and on no other, including the other rows of the same
    /// rule
    #[test]
    fn test_edit_in_progress_is_drawn_on_the_cursor_row_only() {
        let (_dir, mut state) = expanded_state();
        state.cursor = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::TextKnob { .. }))
            .unwrap();
        state.activate_current();
        // The buffer starts from the effective image name
        for _ in 0..state.editing(state.cursor).unwrap().len() {
            state.input_backspace();
        }
        for c in "edited".chars() {
            state.input_push(c);
        }

        let lines = rendered_panel(&state);
        let drawn: Vec<&String> = lines.iter().filter(|line| line.contains('▏')).collect();
        assert_eq!(drawn.len(), 1, "{}", lines.join("\n"));
        assert!(drawn[0].contains("image_name: edited▏"), "{:?}", drawn[0]);
    }

    /// The whole editor drawn at `width` x `height`, without an emblem, one
    /// String per terminal line
    fn rendered_screen(state: &EditorState, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render_ui(f, state, None);
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    /// The keybindings live in the '?' dialog, not along the bottom
    #[test]
    fn test_no_keybinding_legend_until_help_is_opened() {
        let (_dir, mut state) = expanded_state();
        let screen = rendered_screen(&state, 120, 40).join("\n");
        assert!(!screen.contains("write pipeline files"), "{screen}");
        assert!(!screen.contains(" quit"), "{screen}");

        state.help_open = true;
        let screen = rendered_screen(&state, 120, 40).join("\n");
        for (key, _, description) in HELP_BINDINGS {
            assert!(
                screen.contains(&format!("{key}  {description}")),
                "missing {key}:\n{screen}"
            );
        }
    }

    /// The dialog is clamped to the terminal rather than drawn past its edge
    #[test]
    fn test_help_dialog_fits_a_tiny_terminal() {
        let (_dir, mut state) = expanded_state();
        state.help_open = true;
        for (width, height) in [(20, 6), (1, 1), (56, 12)] {
            rendered_screen(&state, width, height);
        }
    }

    /// While a knob is being edited, the rules title says how to get out
    #[test]
    fn test_edit_hint_is_drawn_in_the_rules_title() {
        let (_dir, mut state) = expanded_state();
        state.cursor = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::TextKnob { .. }))
            .unwrap();
        state.activate_current();
        let screen = rendered_screen(&state, 120, 40).join("\n");
        assert!(screen.contains("Enter save · Esc cancel"), "{screen}");
    }

    const CI_YML: &str = ".github/workflows/ci.yml";
    const RELEASE_YML: &str = ".github/workflows/release.yml";

    /// Write what the state's rules generate, as `cibox update` would into
    /// an empty repository, and refresh so the preview sees it
    fn write_pipeline(state: &mut EditorState) {
        let resolved = crate::rules::resolve(&state.facts, &state.config);
        for (path, content) in
            crate::generator::generate(&state.facts, &resolved, state.platforms[0]).unwrap()
        {
            let path = state.working_dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        state.refresh();
    }

    /// Turn the rule `id` on or off through its row, as Space would
    fn toggle_rule(state: &mut EditorState, id: &str) {
        state.cursor = state
            .rows
            .iter()
            .position(|row| matches!(row, Row::Rule(rule) if rule.id == id))
            .unwrap();
        state.activate_current();
    }

    fn preview_text(state: &EditorState) -> String {
        state
            .preview
            .lines
            .iter()
            .map(|(line, kind)| format!("{kind:?}\t{line}\n"))
            .collect()
    }

    /// In a repository without CI config, update would create the whole
    /// pipeline, so the preview shows every file as new
    #[test]
    fn test_preview_shows_the_whole_pipeline_without_one_on_disk() {
        let (_dir, state) = expanded_state();
        let text = preview_text(&state);
        assert!(state.preview_shown());

        let files: Vec<&str> = state
            .preview
            .lines
            .iter()
            .filter(|(_, kind)| *kind == DiffType::File)
            .map(|(line, _)| line.as_str())
            .collect();
        assert_eq!(
            files,
            [
                format!("# ==> {CI_YML} (new) <=="),
                format!("# ==> {RELEASE_YML} (new) <=="),
            ],
            "{text}"
        );
        assert!(
            state
                .preview
                .lines
                .iter()
                .all(|(_, kind)| *kind != DiffType::Removed),
            "{text}"
        );
        assert!(text.contains("cargo test"), "{text}");
    }

    /// With nothing to change the preview is hidden and the rules take the
    /// whole width
    #[test]
    fn test_preview_hidden_when_update_would_change_nothing() {
        let (_dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        assert!(!state.preview_shown(), "{}", preview_text(&state));

        let screen = rendered_screen(&state, 100, 30);
        assert!(!screen.join("\n").contains("Pending"));
        // The rules panel's top-right corner is the screen's right edge
        assert!(screen[4].ends_with('┐'), "{}", screen.join("\n"));
    }

    #[test]
    fn test_preview_drawn_beside_the_rules() {
        let (_dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        toggle_rule(&mut state, "docker-build");
        assert!(state.preview_shown());

        let screen = rendered_screen(&state, 100, 30).join("\n");
        assert!(
            screen.contains("Pending `cibox update` (GitHub"),
            "{screen}"
        );
        assert!(screen.contains(&format!("# ==> {CI_YML} <==")), "{screen}");
    }

    /// A file update would create is shown whole, as added lines, next to
    /// the files that do exist
    #[test]
    fn test_missing_file_is_shown_as_added() {
        let (dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        std::fs::remove_file(dir.path().join(RELEASE_YML)).unwrap();
        state.refresh();

        let text = preview_text(&state);
        let lines = &state.preview.lines;
        assert_eq!(
            lines[0],
            (format!("# ==> {RELEASE_YML} (new) <=="), DiffType::File),
            "{text}"
        );
        assert_eq!(lines[1].1, DiffType::Hunk, "{text}");
        assert!(
            lines[2..].iter().all(|(_, kind)| *kind == DiffType::Added),
            "{text}"
        );
        // ci.yml is already up to date
        assert!(!text.contains(CI_YML), "{text}");
    }

    /// Disabling a rule previews the removal of its job and nothing else
    #[test]
    fn test_disabling_a_rule_previews_only_its_removal() {
        let (_dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        toggle_rule(&mut state, "docker-build");

        let text = preview_text(&state);
        let lines = &state.preview.lines;
        assert_eq!(lines[0], (format!("# ==> {CI_YML} <=="), DiffType::File));
        assert!(
            lines
                .iter()
                .any(|(line, kind)| *kind == DiffType::Removed && line.contains("docker build")),
            "{text}"
        );
        assert!(
            !lines.iter().any(|(_, kind)| *kind == DiffType::Added),
            "{text}"
        );
        // Only the hunk, not the whole file
        let ci = std::fs::read_to_string(state.working_dir.join(CI_YML)).unwrap();
        assert!(lines.len() < ci.lines().count(), "{text}");
    }

    /// The preview merges like `cibox update` does, so a job the user added
    /// by hand is kept rather than shown as removed
    #[test]
    fn test_hand_added_job_is_not_previewed_as_removed() {
        let (dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        let path = dir.path().join(CI_YML);
        let mut doc: serde_yaml::Value =
            serde_yaml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        doc["jobs"].as_mapping_mut().unwrap().insert(
            "my-custom".into(),
            serde_yaml::from_str("runs-on: ubuntu-latest\nsteps:\n  - run: echo custom\n").unwrap(),
        );
        std::fs::write(&path, serde_yaml::to_string(&doc).unwrap()).unwrap();
        state.refresh();
        assert!(!state.preview_shown(), "{}", preview_text(&state));

        toggle_rule(&mut state, "docker-build");
        let text = preview_text(&state);
        assert!(state.preview_shown());
        assert!(
            !state
                .preview
                .lines
                .iter()
                .any(|(line, kind)| *kind == DiffType::Removed
                    && (line.contains("my-custom") || line.contains("echo custom"))),
            "{text}"
        );
    }

    /// When `cibox update` would refuse, the preview says why
    #[test]
    fn test_unmergeable_pipeline_shows_the_update_error() {
        let (dir, mut state) = expanded_state();
        std::fs::create_dir_all(dir.path().join(".github/workflows")).unwrap();
        std::fs::write(dir.path().join(CI_YML), "jobs: [\n").unwrap();
        state.refresh();

        let text = preview_text(&state);
        assert!(state.preview_shown());
        assert!(
            state
                .preview
                .lines
                .iter()
                .all(|(_, kind)| *kind == DiffType::Error),
            "{text}"
        );
        assert!(text.contains("not valid YAML"), "{text}");
    }

    /// With pipelines on disk for two platforms, the preview covers both,
    /// as a flagless `cibox update` would
    #[test]
    fn test_preview_covers_every_existing_platform() {
        let (dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        let resolved = crate::rules::resolve(&state.facts, &state.config);
        for (path, content) in
            crate::generator::generate(&state.facts, &resolved, crate::config::Platform::GitLab)
                .unwrap()
        {
            std::fs::write(dir.path().join(path), content).unwrap();
        }
        state.refresh();
        assert_eq!(
            state.platforms,
            [
                crate::config::Platform::GitHub,
                crate::config::Platform::GitLab
            ]
        );
        assert!(!state.preview_shown(), "{}", preview_text(&state));

        toggle_rule(&mut state, "docker-build");
        let text = preview_text(&state);
        assert!(text.contains(&format!("# ==> {CI_YML} <==")), "{text}");
        assert!(text.contains("# ==> .gitlab-ci.yml <=="), "{text}");

        let screen = rendered_screen(&state, 120, 30).join("\n");
        assert!(
            screen.contains("Pending `cibox update` (GitHub Actions, GitLab CI)"),
            "{screen}"
        );
    }

    /// Focus falls back to the rules when the preview goes away
    #[test]
    fn test_focus_leaves_a_preview_that_disappears() {
        let (_dir, mut state) = expanded_state();
        write_pipeline(&mut state);
        toggle_rule(&mut state, "docker-build");
        state.toggle_focus();
        assert_eq!(state.focus, Focus::Preview);

        toggle_rule(&mut state, "docker-build");
        assert!(!state.preview_shown());
        assert_eq!(state.focus, Focus::Rules);
    }

    /// Every line of a YAML document, styled without a diff background
    fn highlight_yaml(yaml: &str) -> Vec<Line<'static>> {
        yaml.lines()
            .map(|line| highlight_line(line, None))
            .collect()
    }

    /// The preview of rewriting a file holding `old` as `new`
    fn diff_of(old: Option<&str>, new: &str) -> PreviewDiff {
        let update = FileUpdate {
            path: CI_YML.into(),
            existing: old.map(str::to_string),
            change: FileChange::Write {
                content: new.to_string(),
                counts: None,
            },
        };
        let mut preview = PreviewDiff::default();
        preview.push_file(&update, new);
        preview
    }

    /// (text, foreground, background) of every span on a line
    fn spans(line: &Line<'static>) -> Vec<(String, Option<Color>, Option<Color>)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg, s.style.bg))
            .collect()
    }

    #[test]
    fn test_key_value_lines_keep_indentation() {
        let lines = highlight_yaml("jobs:\n    timeout-minutes: 30\n");
        assert_eq!(
            spans(&lines[0]),
            vec![
                ("jobs".to_string(), Some(Color::Cyan), None),
                (":".to_string(), None, None),
            ]
        );
        assert_eq!(
            spans(&lines[1]),
            vec![
                ("    ".to_string(), None, None),
                ("timeout-minutes".to_string(), Some(Color::Cyan), None),
                (":".to_string(), None, None),
                (" ".to_string(), None, None),
                ("30".to_string(), Some(Color::Yellow), None),
            ]
        );
    }

    #[test]
    fn test_scalar_values_are_colored_by_kind() {
        assert_eq!(value_color("\"quoted\""), Some(Color::Green));
        assert_eq!(value_color("'quoted'"), Some(Color::Green));
        assert_eq!(value_color("true"), Some(Color::Magenta));
        assert_eq!(value_color("false"), Some(Color::Magenta));
        assert_eq!(value_color("1.5"), Some(Color::Yellow));
        assert_eq!(value_color("ubuntu-latest"), None);
    }

    #[test]
    fn test_comments_blanks_and_list_items() {
        let lines = highlight_yaml("# a comment\n\n  - cargo test\n  bare\n");
        assert_eq!(
            spans(&lines[0]),
            vec![("# a comment".to_string(), Some(Color::DarkGray), None)]
        );
        assert!(lines[1].spans.is_empty());
        assert_eq!(
            spans(&lines[2]),
            vec![
                ("  ".to_string(), None, None),
                ("- ".to_string(), Some(Color::Yellow), None),
                ("cargo test".to_string(), None, None),
            ]
        );
        // A line with neither a colon nor a bullet is emitted unstyled
        assert_eq!(
            spans(&lines[3]),
            vec![
                ("  ".to_string(), None, None),
                ("bare".to_string(), None, None),
            ]
        );
    }

    #[test]
    fn test_diff_marks_added_and_removed_lines() {
        let preview = diff_of(Some("a: 1\nb: 2\n"), "a: 1\nc: 3\n");
        assert_eq!(
            preview.lines[..2],
            [
                (format!("# ==> {CI_YML} <=="), DiffType::File),
                ("@@ -1,2 +1,2 @@".to_string(), DiffType::Hunk),
            ]
        );
        let lines = preview.visible(0, u16::MAX).split_off(2);
        let backgrounds: Vec<(String, Option<Color>)> = lines
            .iter()
            .map(|l| {
                (
                    l.spans.iter().map(|s| s.content.as_ref()).collect(),
                    l.spans.first().and_then(|s| s.style.bg),
                )
            })
            .collect();
        assert_eq!(
            backgrounds,
            vec![
                ("a: 1".to_string(), None),
                ("b: 2".to_string(), Some(REMOVED_BG)),
                ("c: 3".to_string(), Some(ADDED_BG)),
            ]
        );
        // The background covers the whole line, syntax colors and all
        let added = lines.last().unwrap();
        assert!(added.spans.iter().all(|s| s.style.bg == Some(ADDED_BG)));
        assert_eq!(added.spans[0].style.fg, Some(Color::Cyan));
    }

    /// The panel is handed only the lines that can reach the screen, which
    /// has to draw exactly what the whole document would have drawn — at any
    /// scroll offset, and with lines long enough to wrap onto several rows
    #[test]
    fn test_visible_window_draws_what_the_whole_document_would() {
        let line = |i: usize| format!("  key-{i}: {}\n", "value ".repeat(i % 7));
        let old: String = (0..300).map(line).collect();
        let new: String = (0..300).filter(|i| i % 11 != 0).map(line).collect();
        let preview = diff_of(Some(&old), &new);

        let (width, height) = (24u16, 10u16);
        let draw = |lines: Vec<Line<'static>>, scroll: u16| {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    let paragraph = Paragraph::new(lines)
                        .wrap(Wrap { trim: false })
                        .scroll((scroll, 0));
                    f.render_widget(paragraph, Rect::new(0, 0, width, height));
                })
                .unwrap();
            terminal.backend().buffer().clone()
        };

        for scroll in [0, 1, 9, 40, 300, 700] {
            assert_eq!(
                draw(preview.visible(scroll, height), scroll),
                draw(preview.visible(0, u16::MAX), scroll),
                "scrolled to {scroll}"
            );
        }
    }

    /// Toggling a rule inserts or removes a whole job block. Only that block
    /// may be highlighted: everything after it still lines up and has to come
    /// back `Unchanged`, or the preview turns into a wall of red and green
    /// the moment the user touches anything.
    #[test]
    fn test_only_the_changed_block_is_highlighted() {
        let unchanged_tail = "tail-a: 1\ntail-b: 2\ntail-c: 3\ntail-d: 4\ntail-e: 5\n";
        let block = "  rust-clippy:\n    runs-on: ubuntu-latest\n    container: rust:latest\n    \
                     timeout-minutes: 15\n    steps:\n      - name: Checkout code\n        \
                     uses: actions/checkout@v4\n      - name: Run clippy\n        \
                     run: cargo clippy\n";
        let old = format!("head: 1\n{block}{unchanged_tail}");
        let new = format!("head: 1\n{unchanged_tail}");

        let diff = diff_of(Some(&old), &new).lines;
        let removed: Vec<&str> = diff
            .iter()
            .filter(|(_, d)| *d == DiffType::Removed)
            .map(|(line, _)| line.as_str())
            .collect();
        assert_eq!(removed, block.lines().collect::<Vec<_>>());
        assert!(!diff.iter().any(|(_, d)| *d == DiffType::Added), "{diff:?}");
        // Context around the change is unchanged; beyond it, nothing is shown
        for (i, line) in unchanged_tail.lines().enumerate() {
            let entry = (line.to_string(), DiffType::Unchanged);
            if i < DIFF_CONTEXT {
                assert!(diff.contains(&entry), "{line} should be context: {diff:?}");
            } else {
                assert!(
                    !diff.iter().any(|(l, _)| l == line),
                    "{line} shown: {diff:?}"
                );
            }
        }
    }
}
