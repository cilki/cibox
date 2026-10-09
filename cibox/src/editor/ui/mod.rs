use crate::editor::state::{EditorState, Platform, Row, RuleRow};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffType {
    Unchanged,
    Added,
    Removed,
}

impl DiffType {
    fn background(self) -> Option<Color> {
        match self {
            DiffType::Added => Some(Color::Green),
            DiffType::Removed => Some(Color::Red),
            DiffType::Unchanged => None,
        }
    }
}

pub fn render_ui(f: &mut Frame, state: &EditorState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Information message bar
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Footer
        ])
        .split(f.area());

    // Information message bar (where platform bar was)
    render_info_bar(f, chunks[0], state);

    // Main content (two panels: tree + preview)
    let main_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40), // Left panel (tree)
            Constraint::Percentage(60), // Right panel (preview + platform)
        ])
        .split(chunks[1]);

    render_rules_panel(f, main_chunks[0], state);

    // Right side: preview above platform selector
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),    // Preview
            Constraint::Length(3), // Platform selector
        ])
        .split(main_chunks[1]);

    render_preview_panel(f, right_chunks[0], state);
    render_platform_bar(f, right_chunks[1], state);

    // Footer
    render_footer(f, chunks[2], state);

    // Platform menu overlay (if open)
    if state.platform_menu_open {
        render_platform_menu(f, state);
    }
}

fn render_info_bar(f: &mut Frame, area: Rect, state: &EditorState) {
    let text = if !state.current_item_description.is_empty() {
        state.current_item_description.clone()
    } else {
        "Navigate with ↑↓/jk, toggle rules with Space/Enter".to_string()
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

fn render_platform_bar(f: &mut Frame, area: Rect, state: &EditorState) {
    let inferred = if state.platform == state.inferred_platform {
        " (inferred)"
    } else {
        ""
    };
    let text = format!(
        "Platform: {}{} (press 'p' to change)",
        state.platform.name(),
        inferred
    );

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(paragraph, area);
}

fn render_rules_panel(f: &mut Frame, area: Rect, state: &EditorState) {
    let mut items: Vec<ListItem> = Vec::new();

    for (i, row) in state.rows.iter().enumerate() {
        let is_selected = i == state.cursor;

        let line = match (row, state.editing(row)) {
            // The knob under edit: its own prefix, then the buffer and caret
            (_, Some(input)) => Line::from(vec![
                dim(knob_prefix(row)),
                Span::styled(
                    format!("{}▏", input.buffer),
                    Style::default().fg(Color::Yellow),
                ),
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

    let list = List::new(items).block(
        Block::default()
            .title(" Rules (* = overridden in cibox.ron) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );

    f.render_widget(list, area);
}

/// The dim label a knob row draws before its value, which doubles as the
/// prefix shown while that knob is being edited. Empty for the rows that
/// hold no editable value and so never ask for one.
fn knob_prefix(row: &Row) -> String {
    match row {
        Row::TextKnob { label, .. } => format!("       {label}: "),
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

fn render_preview_panel(f: &mut Frame, area: Rect, state: &EditorState) {
    let preview = Paragraph::new(highlight_yaml(
        &state.yaml_preview,
        state.existing_yaml.as_deref(),
    ))
    .wrap(Wrap { trim: false })
    .scroll((state.preview_scroll, 0));

    let output_path = state.platform.output_path();
    let filename = output_path.to_str().unwrap_or("config.yml");

    let block = Block::default()
        .title(format!(" Preview - {} (Shift+J/K to scroll) ", filename))
        .borders(Borders::ALL);

    f.render_widget(preview.block(block), area);
}

fn render_platform_menu(f: &mut Frame, state: &EditorState) {
    let area = f.area();

    // Center the menu
    let menu_width = 40;
    let menu_height = 8;
    let x = (area.width.saturating_sub(menu_width)) / 2;
    let y = (area.height.saturating_sub(menu_height)) / 2;

    let menu_area = Rect {
        x,
        y,
        width: menu_width,
        height: menu_height,
    };

    // Clear the background
    f.render_widget(Clear, menu_area);

    // Render menu items
    let platforms = Platform::all();
    let items: Vec<ListItem> = platforms
        .iter()
        .enumerate()
        .map(|(i, platform)| {
            let is_selected = i == state.platform_menu_cursor;
            let is_current = *platform == state.platform;

            let style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else if is_current {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };

            let marker = if is_current { "● " } else { "○ " };
            let prefix = if is_selected { "> " } else { "  " };

            ListItem::new(format!("{}{}{}", prefix, marker, platform.name())).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Select Pipeline ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(list, menu_area);
}

/// Line-based diff between the file on disk and the previewed output
fn compute_diff(old: &str, new: &str) -> Vec<(String, DiffType)> {
    TextDiff::from_lines(old, new)
        .iter_all_changes()
        .map(|change| {
            let diff = match change.tag() {
                ChangeTag::Equal => DiffType::Unchanged,
                ChangeTag::Delete => DiffType::Removed,
                ChangeTag::Insert => DiffType::Added,
            };
            (change.value().trim_end_matches('\n').to_string(), diff)
        })
        .collect()
}

/// Syntax-highlight the preview. With `existing` present, every line also
/// carries the background color of its diff status against the file on disk.
fn highlight_yaml(yaml: &str, existing: Option<&str>) -> Vec<Line<'static>> {
    match existing {
        Some(old) => compute_diff(old, yaml)
            .into_iter()
            .map(|(line, diff)| highlight_line(&line, diff.background()))
            .collect(),
        None => yaml
            .lines()
            .map(|line| highlight_line(line, None))
            .collect(),
    }
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

fn render_footer(f: &mut Frame, area: Rect, state: &EditorState) {
    let help_text = if state.input.is_some() {
        vec![
            Span::raw("type to edit | "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" save | "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" cancel"),
        ]
    } else if state.platform_menu_open {
        vec![
            Span::styled("↑↓/jk", Style::default().fg(Color::Blue)),
            Span::raw(" navigate | "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" select | "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" close"),
        ]
    } else {
        vec![
            Span::styled("Space/Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" toggle/edit | "),
            Span::styled("←→/hl", Style::default().fg(Color::Yellow)),
            Span::raw(" options | "),
            Span::styled("d", Style::default().fg(Color::Red)),
            Span::raw(" remove | "),
            Span::styled("↑↓/jk", Style::default().fg(Color::Blue)),
            Span::raw(" navigate | "),
            Span::styled("JK", Style::default().fg(Color::Magenta)),
            Span::raw(" scroll preview | "),
            Span::styled("p", Style::default().fg(Color::Cyan)),
            Span::raw(" platform | "),
            Span::styled("W", Style::default().fg(Color::Green)),
            Span::raw(" write | "),
            Span::styled("q", Style::default().fg(Color::Red)),
            Span::raw(" quit"),
        ]
    };

    let paragraph =
        Paragraph::new(Line::from(help_text)).block(Block::default().borders(Borders::ALL));

    f.render_widget(paragraph, area);
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
        let mut state = EditorState::new(facts, None, dir.path().to_path_buf()).unwrap();
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

    /// (text, foreground, background) of every span on a line
    fn spans(line: &Line<'static>) -> Vec<(String, Option<Color>, Option<Color>)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg, s.style.bg))
            .collect()
    }

    #[test]
    fn test_key_value_lines_keep_indentation() {
        let lines = highlight_yaml("jobs:\n    timeout-minutes: 30\n", None);
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
        let lines = highlight_yaml("# a comment\n\n  - cargo test\n  bare\n", None);
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
        let lines = highlight_yaml("a: 1\nc: 3\n", Some("a: 1\nb: 2\n"));
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
                ("b: 2".to_string(), Some(Color::Red)),
                ("c: 3".to_string(), Some(Color::Green)),
            ]
        );
        // The background covers the whole line, syntax colors and all
        let added = lines.last().unwrap();
        assert!(added.spans.iter().all(|s| s.style.bg == Some(Color::Green)));
        assert_eq!(added.spans[0].style.fg, Some(Color::Cyan));
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

        let diff = compute_diff(&old, &new);
        let removed: Vec<&str> = diff
            .iter()
            .filter(|(_, d)| *d == DiffType::Removed)
            .map(|(line, _)| line.as_str())
            .collect();
        assert_eq!(removed, block.lines().collect::<Vec<_>>());
        assert!(!diff.iter().any(|(_, d)| *d == DiffType::Added), "{diff:?}");
        for line in unchanged_tail.lines() {
            assert!(
                diff.contains(&(line.to_string(), DiffType::Unchanged)),
                "{line} should be unchanged: {diff:?}"
            );
        }
    }
}
