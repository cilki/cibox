use crate::config::{infer_platform, serialize_config, CiboxConfig};
use crate::detection::ProjectFacts;
use crate::error::Result;
use crate::rules::resolve;
use std::path::PathBuf;

pub use crate::config::Platform;

/// One row in the rule checklist
#[derive(Debug, Clone)]
pub struct RuleRow {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub detected: bool,
    pub enabled: bool,
}

impl RuleRow {
    /// Whether cibox.ron overrides detection for this rule
    pub fn overridden(&self) -> bool {
        self.enabled != self.detected
    }
}

pub struct EditorState {
    // Project context
    pub facts: ProjectFacts,
    pub working_dir: PathBuf,

    /// The override file being edited; only deltas from detection live here
    pub config: CiboxConfig,
    /// Whether cibox.ron existed (or has been written) — once true, auto-save
    /// keeps the file up to date even when the delta becomes empty again
    pub has_ron_file: bool,

    pub inferred_platform: Platform,
    pub platform: Platform,

    // Rule checklist
    pub rows: Vec<RuleRow>,
    pub cursor: usize,

    // UI state
    pub platform_menu_open: bool,
    pub platform_menu_cursor: usize,
    pub preview_scroll: u16,
    pub yaml_preview: String,
    pub generation_error: Option<String>,
    pub existing_yaml: Option<String>,
    pub current_item_description: String,

    // Exit flags
    pub should_quit: bool,
    pub should_write: bool,
}

impl EditorState {
    pub fn new(
        facts: ProjectFacts,
        platform_arg: Option<String>,
        working_dir: PathBuf,
    ) -> Result<Self> {
        use std::str::FromStr;

        let ron_path = working_dir.join("cibox.ron");
        let (config, has_ron_file) = match std::fs::read_to_string(&ron_path) {
            Ok(ron_str) => (crate::config::parse_config(&ron_str)?, true),
            Err(_) => (CiboxConfig::default(), false),
        };

        let inferred_platform = infer_platform(&facts);
        let platform = match platform_arg {
            Some(p) => Platform::from_str(&p)
                .map_err(|_| crate::error::unsupported_platform_error(&p))?,
            None => config.platform.unwrap_or(inferred_platform),
        };

        let existing_yaml =
            std::fs::read_to_string(working_dir.join(platform.output_path())).ok();

        let mut state = Self {
            facts,
            working_dir,
            config,
            has_ron_file,
            inferred_platform,
            platform,
            rows: Vec::new(),
            cursor: 0,
            platform_menu_open: false,
            platform_menu_cursor: Platform::all()
                .iter()
                .position(|&p| p == platform)
                .unwrap_or(0),
            preview_scroll: 0,
            yaml_preview: String::new(),
            generation_error: None,
            existing_yaml,
            current_item_description: String::new(),
            should_quit: false,
            should_write: false,
        };

        state.refresh();
        state.update_current_item_description();
        Ok(state)
    }

    /// Re-resolve rules against the current config and regenerate the preview
    pub fn refresh(&mut self) {
        let resolved = resolve(&self.facts, &self.config);
        self.rows = resolved
            .iter()
            .map(|r| RuleRow {
                id: r.rule.id(),
                name: r.rule.name(),
                description: r.rule.description(),
                detected: r.detected,
                enabled: r.enabled,
            })
            .collect();
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }

        self.preview_scroll = 0;
        match crate::generator::generate(&self.facts, &resolved, self.platform) {
            Ok(outputs) => {
                self.yaml_preview = if outputs.len() == 1 {
                    outputs.into_iter().next().unwrap().1
                } else {
                    outputs
                        .into_iter()
                        .map(|(path, content)| format!("# ==> {} <==\n{}", path.display(), content))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                self.generation_error = None;
            }
            Err(e) => {
                self.yaml_preview = format!("# {e}");
                self.generation_error = None;
            }
        }
    }

    pub fn current_row(&self) -> Option<&RuleRow> {
        self.rows.get(self.cursor)
    }

    /// Flip the rule under the cursor. An override matching detection is
    /// removed, so cibox.ron stays delta-only.
    pub fn toggle_current(&mut self) {
        let Some(row) = self.rows.get(self.cursor) else {
            return;
        };
        let new_enabled = !row.enabled;
        let override_value = (new_enabled != row.detected).then_some(new_enabled);
        self.config
            .rules
            .set_enabled_override(row.id, override_value);
        self.refresh();
        self.auto_save_ron();
    }

    pub fn cycle_platform(&mut self) {
        let platforms = Platform::all();
        let current_index = platforms
            .iter()
            .position(|&p| p == self.platform)
            .unwrap_or(0);
        self.switch_to_platform(platforms[(current_index + 1) % platforms.len()]);
    }

    /// Switch the target platform; the choice is stored in cibox.ron only
    /// when it differs from the inferred platform
    pub fn switch_to_platform(&mut self, platform: Platform) {
        self.platform = platform;
        self.config.platform = (platform != self.inferred_platform).then_some(platform);
        self.existing_yaml =
            std::fs::read_to_string(self.working_dir.join(platform.output_path())).ok();
        self.refresh();
        self.auto_save_ron();
    }

    pub fn open_platform_menu(&mut self) {
        self.platform_menu_open = true;
    }

    pub fn close_platform_menu(&mut self) {
        self.platform_menu_open = false;
    }

    pub fn select_platform_from_menu(&mut self) {
        if let Some(&platform) = Platform::all().get(self.platform_menu_cursor) {
            self.switch_to_platform(platform);
        }
        self.platform_menu_open = false;
    }

    pub fn update_current_item_description(&mut self) {
        self.current_item_description = self
            .current_row()
            .map(|row| row.description.to_string())
            .unwrap_or_default();
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(1);
    }

    /// Export the override config as a RON string
    pub fn export_to_ron(&self) -> Result<String> {
        serialize_config(&self.config)
    }

    /// Write cibox.ron when there is anything to say (or the file already
    /// exists and needs updating)
    pub fn auto_save_ron(&mut self) {
        if self.config.is_default() && !self.has_ron_file {
            return;
        }
        let path = self.working_dir.join("cibox.ron");
        if let Ok(ron_str) = self.export_to_ron() {
            if std::fs::write(&path, ron_str).is_ok() {
                self.has_ron_file = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn rust_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        dir
    }

    fn state_for(dir: &std::path::Path) -> EditorState {
        let facts = crate::detection::gather_facts(dir);
        EditorState::new(facts, None, dir.to_path_buf()).unwrap()
    }

    #[test]
    fn test_detected_rules_are_checked() {
        let dir = rust_dir();
        let state = state_for(dir.path());

        let row = |id: &str| state.rows.iter().find(|r| r.id == id).unwrap().clone();
        assert!(row("rust-test").enabled);
        assert!(row("rust-release").enabled);
        assert!(!row("go-test").enabled);
        assert!(!row("docker-build").enabled);
        assert!(state.yaml_preview.contains("cargo test"));
    }

    #[test]
    fn test_toggle_writes_delta_only_ron() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());

        // Disable a detected rule
        state.cursor = state.rows.iter().position(|r| r.id == "rust-fmt").unwrap();
        state.toggle_current();
        assert!(!state.rows[state.cursor].enabled);

        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("rust_fmt"), "{ron_str}");
        assert!(!ron_str.contains("rust_test"), "{ron_str}");

        // Toggling back removes the override entirely
        state.toggle_current();
        assert!(state.config.rules.is_default());
    }

    #[test]
    fn test_platform_override_is_delta_only() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());
        assert_eq!(state.platform, Platform::GitHub);

        state.switch_to_platform(Platform::GitLab);
        assert_eq!(state.config.platform, Some(Platform::GitLab));

        state.switch_to_platform(state.inferred_platform);
        assert_eq!(state.config.platform, None);
    }

    #[test]
    fn test_no_file_written_while_config_is_default() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());
        state.auto_save_ron();
        assert!(!dir.path().join("cibox.ron").exists());
    }

    #[test]
    fn test_loads_existing_overrides() {
        let dir = rust_dir();
        fs::write(
            dir.path().join("cibox.ron"),
            "(rules: (rust_test: (enabled: false)))",
        )
        .unwrap();
        let state = state_for(dir.path());
        let row = state.rows.iter().find(|r| r.id == "rust-test").unwrap();
        assert!(row.detected);
        assert!(!row.enabled);
        assert!(row.overridden());
    }

    #[test]
    fn test_preview_shows_merged_pipeline_for_multiple_environments() {
        let dir = rust_dir();
        fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
        let state = state_for(dir.path());
        // GitHub preview concatenates ci.yml and release.yml
        assert!(state.yaml_preview.contains("cargo test"), "{}", state.yaml_preview);
        assert!(state.yaml_preview.contains("docker build"), "{}", state.yaml_preview);
        assert!(state.yaml_preview.contains("release.yml"), "{}", state.yaml_preview);
    }
}
