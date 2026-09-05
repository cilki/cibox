use crate::editor::config::OptionValue;
use crate::editor::state::{EditorState, Platform, TreeItem};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffType {
    Unchanged,
    Added,
    Removed,
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

    render_presets_panel(f, main_chunks[0], state);

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
        "Navigate with ↑↓/jk, toggle with Space/Enter, expand/collapse with ←→/hl".to_string()
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
    let text = format!(
        "Platform: {} (press 'p' to change)",
        state.target_platform.name()
    );

    let paragraph = Paragraph::new(text)
        .style(Style::default().fg(Color::Cyan))
        .block(Block::default().borders(Borders::ALL));

    f.render_widget(paragraph, area);
}

fn render_presets_panel(f: &mut Frame, area: Rect, state: &EditorState) {
    let mut items: Vec<ListItem> = Vec::new();

    for (i, item) in state.tree_items.iter().enumerate() {
        let is_selected = i == state.tree_cursor;

        let list_item = match item {
            TreeItem::Category(category) => {
                let is_expanded = state.expanded_categories.contains(category);
                let expand_icon = if is_expanded { "▼" } else { "▶" };

                let text_color = if is_selected {
                    Color::Yellow
                } else {
                    Color::White
                };

                let line = Line::from(vec![Span::styled(
                    format!("{} {}", expand_icon, category),
                    Style::default()
                        .fg(text_color)
                        .add_modifier(Modifier::BOLD),
                )]);

                let item_style = if is_selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                ListItem::new(line).style(item_style)
            }
            TreeItem::Preset(preset_id) => {
                let preset = match state.registry.get(preset_id) {
                    Some(p) => p,
                    None => continue,
                };

                let config = state.preset_configs.get(preset_id.as_str());
                let is_expanded = state.expanded_presets.contains(preset_id);
                let has_options_enabled = config
                    .map(|c| {
                        c.values
                            .values()
                            .any(|v| matches!(v, OptionValue::Bool(true)))
                    })
                    .unwrap_or(false);
                let matches_project =
                    preset.matches_project(&state.project_type, &state.working_dir);
                let has_non_defaults = state.has_preset_non_defaults(preset_id);

                let expand_icon = if is_expanded { "▼" } else { "▶" };
                let circle_icon = if has_options_enabled { "●" } else { "○" };

                let circle_color = if has_options_enabled {
                    if matches_project {
                        Color::Green
                    } else {
                        Color::DarkGray
                    }
                } else {
                    Color::White
                };

                let text_color = if is_selected {
                    Color::Yellow
                } else if !has_non_defaults {
                    Color::DarkGray
                } else {
                    Color::White
                };

                let line = Line::from(vec![
                    Span::styled(
                        format!("  {} ", expand_icon),
                        Style::default().fg(text_color),
                    ),
                    Span::styled(circle_icon, Style::default().fg(circle_color)),
                    Span::styled(
                        format!(" {}", preset.preset_name()),
                        Style::default().fg(text_color),
                    ),
                ]);

                let item_style = if is_selected {
                    Style::default().add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                ListItem::new(line).style(item_style)
            }
            TreeItem::Field(preset_id, field_id) => {
                let preset = match state.registry.get(preset_id) {
                    Some(p) => p,
                    None => continue,
                };

                let config = match state.preset_configs.get(preset_id.as_str()) {
                    Some(c) => c,
                    None => continue,
                };

                let value = match config.get(field_id) {
                    Some(v) => v,
                    None => continue,
                };

                // Find the field metadata to get the display name
                let fields = preset.fields();
                let field_meta = fields.iter().find(|f| &f.id == field_id);

                let display_name = field_meta
                    .map(|f| f.display_name.as_str())
                    .unwrap_or(field_id);
                let is_non_default = state.is_option_non_default(preset_id, field_id);

                let display_text = match value {
                    OptionValue::Bool(b) => {
                        let checkbox = if *b { "[✓]" } else { "[ ]" };
                        format!("      {} {}", checkbox, display_name)
                    }
                    OptionValue::Enum { selected, .. } => {
                        format!("      {} ({})", display_name, selected)
                    }
                    OptionValue::String(s) => {
                        format!("      {}: {}", display_name, s)
                    }
                    OptionValue::Int(n) => {
                        format!("      {}: {}", display_name, n)
                    }
                };

                let text_color = if is_selected {
                    Color::Yellow
                } else if !is_non_default {
                    Color::DarkGray
                } else {
                    Color::White
                };

                let item_style = if is_selected {
                    Style::default().fg(text_color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(text_color)
                };

                ListItem::new(display_text).style(item_style)
            }
        };

        items.push(list_item);
    }

    let list = List::new(items).block(
        Block::default()
            .title(" Presets ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );

    f.render_widget(list, area);
}

fn render_preview_panel(f: &mut Frame, area: Rect, state: &EditorState) {
    let preview = if let Some(error) = &state.generation_error {
        Paragraph::new(format!("Error: {}", error))
            .style(Style::default().fg(Color::Red))
            .wrap(Wrap { trim: true })
            .scroll((state.preview_scroll, 0))
    } else {
        // Apply syntax highlighting to YAML with diff support
        let lines = if let Some(existing) = &state.existing_yaml {
            highlight_yaml_with_diff(&state.yaml_preview, existing)
        } else {
            highlight_yaml(&state.yaml_preview)
        };
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((state.preview_scroll, 0))
    };

    let output_path = state.target_platform.output_path();
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
            let is_current = *platform == state.target_platform;

            let style = if is_selected {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let marker = if is_current { "● " } else { "  " };
            let prefix = if is_selected { "> " } else { "  " };

            ListItem::new(format!("{}{}{}", prefix, marker, platform.name())).style(style)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .title(" Select Platform ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .style(Style::default().bg(Color::Black)),
    );

    f.render_widget(list, menu_area);
}

/// Compute a simple line-based diff between old and new text
fn compute_diff(old: &str, new: &str) -> Vec<(String, DiffType)> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();

    let mut result = Vec::new();
    let mut old_idx = 0;
    let mut new_idx = 0;

    while old_idx < old_lines.len() || new_idx < new_lines.len() {
        if old_idx >= old_lines.len() {
            // Remaining lines are added
            result.push((new_lines[new_idx].to_string(), DiffType::Added));
            new_idx += 1;
        } else if new_idx >= new_lines.len() {
            // Remaining lines are removed
            result.push((old_lines[old_idx].to_string(), DiffType::Removed));
            old_idx += 1;
        } else if old_lines[old_idx] == new_lines[new_idx] {
            // Lines match
            result.push((new_lines[new_idx].to_string(), DiffType::Unchanged));
            old_idx += 1;
            new_idx += 1;
        } else {
            // Lines differ - look ahead to see if we can find a match
            let mut found_match = false;

            // Look ahead in new for current old line (removed line)
            for i in (new_idx + 1)..(new_idx + 5).min(new_lines.len()) {
                if old_lines[old_idx] == new_lines[i] {
                    // Found old line later in new, so lines between are added
                    while new_idx < i {
                        result.push((new_lines[new_idx].to_string(), DiffType::Added));
                        new_idx += 1;
                    }
                    found_match = true;
                    break;
                }
            }

            if !found_match {
                // Look ahead in old for current new line (added line)
                for i in (old_idx + 1)..(old_idx + 5).min(old_lines.len()) {
                    if old_lines[i] == new_lines[new_idx] {
                        // Found new line later in old, so lines between are removed
                        while old_idx < i {
                            result.push((old_lines[old_idx].to_string(), DiffType::Removed));
                            old_idx += 1;
                        }
                        found_match = true;
                        break;
                    }
                }
            }

            if !found_match {
                // No match found, treat as changed (removed + added)
                result.push((old_lines[old_idx].to_string(), DiffType::Removed));
                result.push((new_lines[new_idx].to_string(), DiffType::Added));
                old_idx += 1;
                new_idx += 1;
            }
        }
    }

    result
}

/// Highlight YAML with diff information
fn highlight_yaml_with_diff(new_yaml: &str, old_yaml: &str) -> Vec<Line<'static>> {
    let diff = compute_diff(old_yaml, new_yaml);
    let mut lines = Vec::new();

    for (line_text, diff_type) in diff {
        let bg_color = match diff_type {
            DiffType::Added => Some(Color::Green),
            DiffType::Removed => Some(Color::Red),
            DiffType::Unchanged => None,
        };

        // Apply YAML syntax highlighting to the line
        let highlighted_line = highlight_yaml_line_owned(line_text, bg_color);
        lines.push(highlighted_line);
    }

    lines
}

/// Highlight a single YAML line with optional background color (owned version for diff)
fn highlight_yaml_line_owned(line: String, bg_color: Option<Color>) -> Line<'static> {
    let trimmed_start = line.trim_start();

    if trimmed_start.is_empty() {
        return Line::from("");
    }

    // Comment lines
    if trimmed_start.starts_with('#') {
        let mut style = Style::default().fg(Color::DarkGray);
        if let Some(bg) = bg_color {
            style = style.bg(bg);
        }
        return Line::from(Span::styled(line, style));
    }

    // Parse the line into spans
    let mut spans = Vec::new();
    let indent = line.len() - trimmed_start.len();

    // Add indentation
    if indent > 0 {
        let mut style = Style::default();
        if let Some(bg) = bg_color {
            style = style.bg(bg);
        }
        spans.push(Span::styled(" ".repeat(indent), style));
    }

    // Key-value pairs
    if let Some(colon_pos) = trimmed_start.find(':') {
        let key = trimmed_start[..colon_pos].to_string();
        let rest = &trimmed_start[colon_pos..];

        // Key (cyan)
        let mut key_style = Style::default().fg(Color::Cyan);
        if let Some(bg) = bg_color {
            key_style = key_style.bg(bg);
        }
        spans.push(Span::styled(key, key_style));

        // Colon
        let mut colon_style = Style::default();
        if let Some(bg) = bg_color {
            colon_style = colon_style.bg(bg);
        }
        spans.push(Span::styled(":".to_string(), colon_style));

        if rest.len() > 1 {
            let value = rest[1..].trim_start();

            // Space before value
            let mut space_style = Style::default();
            if let Some(bg) = bg_color {
                space_style = space_style.bg(bg);
            }
            spans.push(Span::styled(" ".to_string(), space_style));

            // Check for special values
            let mut value_style = if value.starts_with('"') || value.starts_with('\'') {
                // String value (green)
                Style::default().fg(Color::Green)
            } else if value == "true" || value == "false" {
                // Boolean (magenta)
                Style::default().fg(Color::Magenta)
            } else if value.parse::<f64>().is_ok() {
                // Number (yellow)
                Style::default().fg(Color::Yellow)
            } else {
                // Other value
                Style::default()
            };

            if let Some(bg) = bg_color {
                value_style = value_style.bg(bg);
            }

            if !value.is_empty() {
                spans.push(Span::styled(value.to_string(), value_style));
            }
        }
    } else if let Some(rest) = trimmed_start.strip_prefix("- ") {
        // List item
        let mut bullet_style = Style::default().fg(Color::Yellow);
        if let Some(bg) = bg_color {
            bullet_style = bullet_style.bg(bg);
        }
        spans.push(Span::styled("- ".to_string(), bullet_style));

        let mut text_style = Style::default();
        if let Some(bg) = bg_color {
            text_style = text_style.bg(bg);
        }
        spans.push(Span::styled(rest.to_string(), text_style));
    } else {
        // Other lines
        let mut style = Style::default();
        if let Some(bg) = bg_color {
            style = style.bg(bg);
        }
        spans.push(Span::styled(trimmed_start.to_string(), style));
    }

    Line::from(spans)
}

fn highlight_yaml(yaml: &str) -> Vec<Line<'_>> {
    let mut lines = Vec::new();

    for line in yaml.lines() {
        let trimmed = line.trim_start();

        if trimmed.is_empty() {
            lines.push(Line::from(""));
            continue;
        }

        // Comment lines
        if trimmed.starts_with('#') {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            continue;
        }

        // Parse the line into spans
        let mut spans = Vec::new();
        let indent = line.len() - trimmed.len();

        // Add indentation
        if indent > 0 {
            spans.push(Span::raw(" ".repeat(indent)));
        }

        // Key-value pairs
        if let Some(colon_pos) = trimmed.find(':') {
            let key = &trimmed[..colon_pos];
            let rest = &trimmed[colon_pos..];

            // Key (cyan)
            spans.push(Span::styled(
                key.to_string(),
                Style::default().fg(Color::Cyan),
            ));

            // Colon
            spans.push(Span::raw(":"));

            if rest.len() > 1 {
                let value = &rest[1..].trim_start();

                // Check for special values
                if value.starts_with('"') || value.starts_with('\'') {
                    // String value (green)
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        value.to_string(),
                        Style::default().fg(Color::Green),
                    ));
                } else if *value == "true" || *value == "false" {
                    // Boolean (magenta)
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        value.to_string(),
                        Style::default().fg(Color::Magenta),
                    ));
                } else if value.parse::<f64>().is_ok() {
                    // Number (yellow)
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        value.to_string(),
                        Style::default().fg(Color::Yellow),
                    ));
                } else if !value.is_empty() {
                    // Other value
                    spans.push(Span::raw(" "));
                    spans.push(Span::raw(value.to_string()));
                }
            }
        } else if let Some(rest) = trimmed.strip_prefix("- ") {
            // List item
            spans.push(Span::styled(
                "- ".to_string(),
                Style::default().fg(Color::Yellow),
            ));
            spans.push(Span::raw(rest.to_string()));
        } else {
            // Other lines
            spans.push(Span::raw(trimmed.to_string()));
        }

        lines.push(Line::from(spans));
    }

    lines
}

fn render_footer(f: &mut Frame, area: Rect, state: &EditorState) {
    let help_text = if state.platform_menu_open {
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
            Span::styled("←→/hl", Style::default().fg(Color::Blue)),
            Span::raw(" expand/collapse | "),
            Span::styled("Space/Enter", Style::default().fg(Color::Yellow)),
            Span::raw(" toggle | "),
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
