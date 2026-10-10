use crate::config::{infer_platform, serialize_config, CiboxConfig, DockerPlatform};
use crate::detection::ProjectFacts;
use crate::error::Result;
use crate::rules::{clean_versions, resolve};
use std::collections::HashSet;
use std::path::PathBuf;

pub use crate::config::Platform;

/// Test rules with a versions knob, and the default image shown when unset
const VERSIONED_RULES: [(&str, &str); 4] = [
    ("rust-test", crate::rules::rust::IMAGE),
    ("python-test", crate::rules::python::IMAGE),
    ("go-test", crate::rules::go::IMAGE),
    ("node-test", crate::rules::node::IMAGE),
];

/// A rule line in the checklist
#[derive(Debug, Clone)]
pub struct RuleRow {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub detected: bool,
    pub enabled: bool,
    /// Whether this rule has config options to expand
    pub expandable: bool,
    pub expanded: bool,
}

impl RuleRow {
    /// Whether cibox.ron overrides detection for this rule
    pub fn overridden(&self) -> bool {
        self.enabled != self.detected
    }
}

/// One row in the rules panel: a rule, or one config option of an expanded rule
#[derive(Debug, Clone)]
pub enum Row {
    Rule(RuleRow),
    /// A string knob (e.g. image_name)
    TextKnob {
        rule_id: &'static str,
        label: &'static str,
        description: &'static str,
        /// The cibox.ron override, if any
        override_value: Option<String>,
        /// The value in effect (facts-derived default when not overridden)
        effective: String,
    },
    /// A checkbox for one of the boolean knobs
    Checkbox { knob: Checkbox, value: bool },
    /// One configured toolchain version under an expanded test rule, or —
    /// with `index: None` — the trailing "add a version" action row
    Version {
        rule_id: &'static str,
        index: Option<usize>,
        value: String,
        /// Image used when the rule has no versions configured
        default_image: &'static str,
    },
}

/// A boolean knob the checklist renders as a checkbox. Flipping one is the
/// whole of what activating its row does, so the row needs to name which.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Checkbox {
    /// docker-release's `sync_readme`
    SyncReadme,
    /// One entry of docker-release's `platforms`
    Arch(DockerPlatform),
}

impl Checkbox {
    pub fn label(&self) -> &'static str {
        match self {
            Checkbox::SyncReadme => "sync_readme",
            Checkbox::Arch(arch) => arch.as_str(),
        }
    }

    pub fn description(&self) -> String {
        match self {
            Checkbox::SyncReadme => "Sync README.md to the Docker Hub repository description on \
                                     release; needs DOCKER_USERNAME/DOCKER_PASSWORD. Docker Hub \
                                     only. Press Enter to toggle."
                .to_string(),
            Checkbox::Arch(arch) => {
                format!("Include {} in the released multi-arch image", arch.as_str())
            }
        }
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
    pub rows: Vec<Row>,
    pub cursor: usize,
    /// Ids of rules whose config options are shown
    pub expanded: HashSet<&'static str>,
    /// Text being typed into a knob row, if an edit is in progress.
    ///
    /// Which knob that is doesn't need recording: an edit starts on the row
    /// under the cursor, and while one is in progress the key handler feeds
    /// every keystroke to the buffer — so nothing can move the cursor or
    /// rebuild the rows until it is committed or cancelled. The row the
    /// buffer belongs to is therefore always `rows[cursor]`, and that row
    /// already says which knob it edits.
    pub input: Option<String>,

    // UI state
    pub platform_menu_open: bool,
    pub platform_menu_cursor: usize,
    pub preview_scroll: u16,
    pub yaml_preview: String,
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
            None => inferred_platform,
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
            expanded: HashSet::new(),
            input: None,
            platform_menu_open: false,
            platform_menu_cursor: Platform::all()
                .iter()
                .position(|&p| p == platform)
                .unwrap_or(0),
            preview_scroll: 0,
            yaml_preview: String::new(),
            existing_yaml,
            current_item_description: String::new(),
            should_quit: false,
            should_write: false,
        };

        state.refresh();
        state.update_current_item_description();
        Ok(state)
    }

    /// The image name detection would use without any override
    fn default_image(&self) -> String {
        crate::rules::docker_image(&self.facts, &CiboxConfig::default())
    }

    /// The image name in effect, as the docker rules will see it
    fn effective_image(&self) -> String {
        crate::rules::docker_image(&self.facts, &self.config)
    }

    /// The config-option rows of one rule, in display order: the toolchain
    /// versions of a test rule, or the image name (plus docker-release's
    /// booleans) of a docker rule. Empty for every other rule, which is also
    /// what makes it unexpandable — one list of knobs per rule, rather than a
    /// second classification to keep in step with this one.
    fn knob_rows(&self, rule_id: &'static str) -> Vec<Row> {
        if let Some(&(_, default_image)) = VERSIONED_RULES.iter().find(|(id, _)| *id == rule_id) {
            let versions = self
                .config
                .versions_override(rule_id)
                .cloned()
                .unwrap_or_default();
            return versions
                .into_iter()
                .enumerate()
                .map(|(index, value)| Row::Version {
                    rule_id,
                    index: Some(index),
                    value,
                    default_image,
                })
                // The trailing "add a version" action row
                .chain([Row::Version {
                    rule_id,
                    index: None,
                    value: String::new(),
                    default_image,
                }])
                .collect();
        }

        if !matches!(rule_id, "docker-build" | "docker-release") {
            return Vec::new();
        }
        let mut rows = vec![Row::TextKnob {
            rule_id,
            label: "image_name",
            description: "Image name, e.g. \"fossable/cibox\"; applies to both docker \
                          rules. Press Enter to edit.",
            override_value: match rule_id {
                "docker-build" => self.config.docker_build.image_name.clone(),
                _ => self.config.docker_release.image_name.clone(),
            },
            effective: self.effective_image(),
        }];
        if rule_id == "docker-release" {
            rows.push(Row::Checkbox {
                knob: Checkbox::SyncReadme,
                value: self.config.docker_release.sync_readme.unwrap_or(false),
            });
            let selected = self
                .config
                .docker_release
                .platforms
                .clone()
                .unwrap_or_default();
            rows.extend(DockerPlatform::ALL.map(|arch| Row::Checkbox {
                knob: Checkbox::Arch(arch),
                value: selected.contains(&arch),
            }));
        }
        rows
    }

    /// Re-resolve rules against the current config and regenerate the preview
    pub fn refresh(&mut self) {
        let resolved = resolve(&self.facts, &self.config);
        self.rows = Vec::new();
        for r in &resolved {
            let id = r.rule.id();
            let knobs = self.knob_rows(id);
            let expanded = !knobs.is_empty() && self.expanded.contains(id);
            self.rows.push(Row::Rule(RuleRow {
                id,
                name: r.rule.name(),
                description: r.rule.description(),
                detected: r.detected,
                enabled: r.enabled,
                expandable: !knobs.is_empty(),
                expanded,
            }));
            if expanded {
                self.rows.extend(knobs);
            }
        }
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }

        self.preview_scroll = 0;
        self.yaml_preview = match crate::generator::generate(&self.facts, &resolved, self.platform)
        {
            // A single file is previewed verbatim; several get path banners
            Ok(outputs) if outputs.len() == 1 => outputs.into_iter().next().unwrap().1,
            Ok(outputs) => outputs
                .into_iter()
                .map(|(path, content)| format!("# ==> {} <==\n{}", path.display(), content))
                .collect::<Vec<_>>()
                .join("\n"),
            // Nothing enabled, or the platform refuses a job: say so inline
            Err(e) => format!("# {e}"),
        };
    }

    pub fn current_row(&self) -> Option<&Row> {
        self.rows.get(self.cursor)
    }

    /// The text being typed into the row at `index`, if that is the row the
    /// edit in progress belongs to — which is the one under the cursor.
    pub fn editing(&self, index: usize) -> Option<&str> {
        (index == self.cursor)
            .then_some(self.input.as_deref())
            .flatten()
    }

    /// Activate the row under the cursor: toggle a rule or a checkbox, or
    /// start editing a text knob from its current value
    pub fn activate_current(&mut self) {
        match self.rows.get(self.cursor).cloned() {
            Some(Row::Rule(rule)) => self.toggle_rule(&rule),
            Some(Row::Checkbox { knob, .. }) => self.toggle_checkbox(knob),
            Some(Row::TextKnob { effective, .. }) => self.input = Some(effective),
            Some(Row::Version { value, .. }) => self.input = Some(value),
            None => {}
        }
    }

    /// Flip a rule's enabled state. An override matching detection is
    /// removed, so cibox.ron stays delta-only.
    fn toggle_rule(&mut self, rule: &RuleRow) {
        let new_enabled = !rule.enabled;
        let override_value = (new_enabled != rule.detected).then_some(new_enabled);
        self.config
            .set_enabled_override(rule.id, override_value);
        self.refresh();
        self.auto_save_ron();
    }

    /// Flip one boolean knob. The off state matches the default in both
    /// cases — `sync_readme` unset, `platforms` empty — so it collapses back
    /// to "unset" and cibox.ron stays delta-only.
    pub fn toggle_checkbox(&mut self, knob: Checkbox) {
        match knob {
            Checkbox::SyncReadme => {
                let new = !self.config.docker_release.sync_readme.unwrap_or(false);
                self.config.docker_release.sync_readme = new.then_some(true);
            }
            Checkbox::Arch(arch) => {
                let mut selected = self
                    .config
                    .docker_release
                    .platforms
                    .clone()
                    .unwrap_or_default();
                if let Some(pos) = selected.iter().position(|p| *p == arch) {
                    selected.remove(pos);
                } else {
                    selected.push(arch);
                }
                // Stored in ALL order, so the file doesn't record click order
                let selected: Vec<DockerPlatform> = DockerPlatform::ALL
                    .iter()
                    .copied()
                    .filter(|p| selected.contains(p))
                    .collect();
                self.config.docker_release.platforms = (!selected.is_empty()).then_some(selected);
            }
        }
        self.refresh();
        self.auto_save_ron();
    }

    /// Commit the text being typed into the knob row under the cursor. A
    /// value matching the facts-derived default (or an empty one) removes the
    /// override.
    pub fn commit_input(&mut self) {
        let Some(buffer) = self.input.take() else {
            return;
        };
        let value = buffer.trim().to_string();
        match self.rows.get(self.cursor).cloned() {
            Some(Row::TextKnob { rule_id, .. }) => {
                // The typed name is written to cibox.ron and from there into
                // a shell command, so coerce it into a valid reference rather
                // than saving something the config parser would reject
                let override_value = (!value.is_empty())
                    .then(|| crate::config::image::coerce_reference(&value))
                    .filter(|value| *value != self.default_image());
                match rule_id {
                    "docker-build" => self.config.docker_build.image_name = override_value,
                    "docker-release" => self.config.docker_release.image_name = override_value,
                    _ => {}
                }
            }
            Some(Row::Version { rule_id, index, .. }) => {
                let mut versions = self
                    .config
                    .versions_override(rule_id)
                    .cloned()
                    .unwrap_or_default();
                match index {
                    // Committing an empty value removes the entry
                    Some(i) if value.is_empty() => {
                        if i < versions.len() {
                            versions.remove(i);
                        }
                    }
                    Some(i) => {
                        if i < versions.len() {
                            versions[i] = value;
                        }
                    }
                    None if value.is_empty() => {}
                    None => versions.push(value),
                }
                // Normalize exactly as rules::resolve would, so what the
                // preview shows is what the file means
                let versions = clean_versions(Some(&versions));
                self.config
                    .set_versions_override(rule_id, (!versions.is_empty()).then_some(versions));
            }
            _ => return,
        }
        self.refresh();
        self.update_current_item_description();
        self.auto_save_ron();
    }

    /// Remove the toolchain version under the cursor; an empty list collapses
    /// back to "unset" so cibox.ron stays delta-only
    pub fn delete_current_version(&mut self) {
        let Some(Row::Version {
            rule_id,
            index: Some(index),
            ..
        }) = self.rows.get(self.cursor).cloned()
        else {
            return;
        };
        let mut versions = self
            .config
            .versions_override(rule_id)
            .cloned()
            .unwrap_or_default();
        if index < versions.len() {
            versions.remove(index);
        }
        self.config
            .set_versions_override(rule_id, (!versions.is_empty()).then_some(versions));
        self.refresh();
        self.update_current_item_description();
        self.auto_save_ron();
    }

    pub fn cancel_input(&mut self) {
        self.input = None;
    }

    pub fn input_push(&mut self, c: char) {
        if let Some(buffer) = &mut self.input {
            buffer.push(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if let Some(buffer) = &mut self.input {
            buffer.pop();
        }
    }

    /// Show the config options of the rule under the cursor
    pub fn expand_current(&mut self) {
        if let Some(Row::Rule(rule)) = self.rows.get(self.cursor) {
            if rule.expandable && self.expanded.insert(rule.id) {
                self.refresh();
            }
        }
    }

    /// Index of the rule row owning the row at `idx`
    fn parent_rule_index(&self, mut idx: usize) -> Option<usize> {
        loop {
            if matches!(self.rows.get(idx)?, Row::Rule(_)) {
                return Some(idx);
            }
            idx = idx.checked_sub(1)?;
        }
    }

    /// Hide the config options of the rule under the cursor (jumping to the
    /// parent rule first when on a child row)
    pub fn collapse_current(&mut self) {
        let Some(parent) = self.parent_rule_index(self.cursor) else {
            return;
        };
        let Some(Row::Rule(rule)) = self.rows.get(parent) else {
            return;
        };
        if self.expanded.remove(rule.id) {
            self.cursor = parent;
            self.refresh();
            self.update_current_item_description();
        }
    }

    pub fn cycle_platform(&mut self) {
        let platforms = Platform::all();
        let current_index = platforms
            .iter()
            .position(|&p| p == self.platform)
            .unwrap_or(0);
        self.switch_to_platform(platforms[(current_index + 1) % platforms.len()]);
    }

    /// Switch the target platform for this session; the choice is not
    /// persisted — cibox.ron holds only rule overrides
    pub fn switch_to_platform(&mut self, platform: Platform) {
        self.platform = platform;
        self.existing_yaml =
            std::fs::read_to_string(self.working_dir.join(platform.output_path())).ok();
        self.refresh();
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
        self.current_item_description = match self.current_row() {
            Some(Row::Rule(row)) => row.description.to_string(),
            Some(Row::TextKnob { description, .. }) => description.to_string(),
            Some(Row::Checkbox { knob, .. }) => knob.description(),
            Some(Row::Version { index: Some(_), .. }) => {
                "Toolchain version run as one matrix leg; Enter to edit, d to remove".to_string()
            }
            Some(Row::Version {
                index: None,
                default_image,
                ..
            }) => format!(
                "Add a toolchain version to test against; unset = default image {default_image}"
            ),
            None => String::new(),
        };
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

    fn docker_dir() -> tempfile::TempDir {
        let dir = rust_dir();
        fs::write(dir.path().join("Dockerfile"), "FROM rust:latest\n").unwrap();
        dir
    }

    fn state_for(dir: &std::path::Path) -> EditorState {
        let facts = crate::detection::gather_facts(dir);
        EditorState::new(facts, None, dir.to_path_buf()).unwrap()
    }

    fn rule_row<'a>(state: &'a EditorState, id: &str) -> &'a RuleRow {
        state
            .rows
            .iter()
            .find_map(|r| match r {
                Row::Rule(row) if row.id == id => Some(row),
                _ => None,
            })
            .unwrap()
    }

    /// Backspace over the whole prefilled buffer of the edit in progress
    fn clear_input(state: &mut EditorState) {
        for _ in 0..state.input.as_deref().unwrap_or_default().len() {
            state.input_backspace();
        }
    }

    fn rule_index(state: &EditorState, id: &str) -> usize {
        state
            .rows
            .iter()
            .position(|r| matches!(r, Row::Rule(row) if row.id == id))
            .unwrap()
    }

    #[test]
    fn test_detected_rules_are_checked() {
        let dir = rust_dir();
        let state = state_for(dir.path());

        assert!(rule_row(&state, "rust-test").enabled);
        assert!(rule_row(&state, "rust-release").enabled);
        assert!(!rule_row(&state, "go-test").enabled);
        assert!(!rule_row(&state, "docker-build").enabled);
        assert!(state.yaml_preview.contains("cargo test"));
    }

    #[test]
    fn test_toggle_writes_delta_only_ron() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());

        // Disable a detected rule
        state.cursor = rule_index(&state, "rust-fmt");
        state.activate_current();
        assert!(!rule_row(&state, "rust-fmt").enabled);

        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("rust_fmt"), "{ron_str}");
        assert!(!ron_str.contains("rust_test"), "{ron_str}");

        // Toggling back removes the override entirely
        state.activate_current();
        assert!(state.config.is_default());
    }

    #[test]
    fn test_platform_switch_is_session_only() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());
        assert_eq!(state.platform, Platform::GitHub);

        // Switching platforms never touches cibox.ron — it only holds rules
        state.switch_to_platform(Platform::GitLab);
        assert_eq!(state.platform, Platform::GitLab);
        assert!(state.config.is_default());
        assert!(!dir.path().join("cibox.ron").exists());
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
            "(rust_test: (enabled: false))",
        )
        .unwrap();
        let state = state_for(dir.path());
        let row = rule_row(&state, "rust-test");
        assert!(row.detected);
        assert!(!row.enabled);
        assert!(row.overridden());
    }

    #[test]
    fn test_preview_shows_merged_pipeline_for_multiple_environments() {
        let dir = docker_dir();
        let state = state_for(dir.path());
        // GitHub preview concatenates ci.yml and release.yml
        assert!(state.yaml_preview.contains("cargo test"), "{}", state.yaml_preview);
        assert!(state.yaml_preview.contains("docker build"), "{}", state.yaml_preview);
        assert!(state.yaml_preview.contains("release.yml"), "{}", state.yaml_preview);
    }

    #[test]
    fn test_expand_shows_config_option_rows() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());
        let collapsed_len = state.rows.len();

        // Only expandable rules react to expand
        state.cursor = rule_index(&state, "rust-fmt");
        state.expand_current();
        assert_eq!(
            state.rows.len(),
            collapsed_len,
            "non-expandable rule must not grow the list"
        );

        state.cursor = rule_index(&state, "docker-release");
        state.expand_current();
        assert!(rule_row(&state, "docker-release").expanded);
        let idx = rule_index(&state, "docker-release");
        assert!(matches!(
            state.rows[idx + 1],
            Row::TextKnob { label: "image_name", .. }
        ));
        assert!(matches!(
            state.rows[idx + 2],
            Row::Checkbox {
                knob: Checkbox::SyncReadme,
                ..
            }
        ));
        // image_name + sync_readme + one row per DockerPlatform
        assert_eq!(state.rows.len(), collapsed_len + 2 + DockerPlatform::ALL.len());

        // Collapsing from a child row jumps back to the parent
        state.cursor = idx + 2;
        state.collapse_current();
        assert_eq!(state.cursor, rule_index(&state, "docker-release"));
        assert_eq!(state.rows.len(), collapsed_len);
    }

    #[test]
    fn test_sync_readme_toggle_is_delta_only() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());

        state.cursor = rule_index(&state, "docker-release");
        state.expand_current();
        let idx = rule_index(&state, "docker-release");
        assert!(matches!(
            state.rows[idx + 2],
            Row::Checkbox {
                knob: Checkbox::SyncReadme,
                value: false
            }
        ));

        state.cursor = idx + 2;
        state.activate_current();
        assert_eq!(state.config.docker_release.sync_readme, Some(true));
        assert!(matches!(
            state.rows[idx + 2],
            Row::Checkbox {
                knob: Checkbox::SyncReadme,
                value: true
            }
        ));
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("sync_readme"), "{ron_str}");
        assert!(state.yaml_preview.contains("docker-pushrm"), "{}", state.yaml_preview);

        // Toggling back off matches the default, so the override disappears
        state.activate_current();
        assert!(state.config.is_default());
    }

    #[test]
    fn test_version_list_editing() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());

        // Expanding a versioned test rule shows only the add row
        state.cursor = rule_index(&state, "rust-test");
        state.expand_current();
        let idx = rule_index(&state, "rust-test");
        assert!(matches!(state.rows[idx + 1], Row::Version { index: None, .. }));

        // Adding a version through the add row
        state.cursor = idx + 1;
        state.activate_current();
        // The add row holds no value, so the edit starts from an empty buffer
        assert_eq!(state.editing(state.cursor), Some(""));
        for c in "1.85".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.rust_test.versions,
            Some(vec!["1.85".to_string()])
        );
        assert!(matches!(
            &state.rows[idx + 1],
            Row::Version { index: Some(0), value, .. } if value == "1.85"
        ));

        // A second version turns the preview into a matrix
        state.cursor = idx + 2;
        state.activate_current();
        for c in "nightly".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert!(state.yaml_preview.contains("matrix"), "{}", state.yaml_preview);

        // cibox.ron stays delta-only
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("versions"), "{ron_str}");
        assert!(!ron_str.contains("rust_fmt"), "{ron_str}");

        // Editing in place replaces the entry; duplicates collapse
        state.cursor = idx + 1;
        state.activate_current();
        // ...prefilled with the entry under the cursor
        assert_eq!(state.editing(state.cursor), Some("1.85"));
        clear_input(&mut state);
        for c in "nightly".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.rust_test.versions,
            Some(vec!["nightly".to_string()])
        );

        // Removing the last version collapses the override entirely
        state.cursor = idx + 1;
        state.delete_current_version();
        assert!(state.config.is_default());
    }

    /// A rule listed here but missing from the `versions` list in
    /// `rule_overrides!` would render an editable version list whose edits go
    /// nowhere, so the two have to agree
    #[test]
    fn test_every_versioned_rule_has_a_versions_slot() {
        for (id, _) in VERSIONED_RULES {
            let mut config = CiboxConfig::default();
            config.set_versions_override(id, Some(vec!["1".to_string()]));
            assert_eq!(
                config.versions_override(id),
                Some(&vec!["1".to_string()]),
                "rule {id} is missing from the CiboxConfig versions mapping"
            );
        }
    }

    #[test]
    fn test_delete_only_acts_on_version_rows() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());
        state.cursor = rule_index(&state, "rust-test");
        state.delete_current_version();
        assert!(state.config.is_default());
    }

    /// Index of the row holding a given checkbox
    fn checkbox_index(state: &EditorState, knob: Checkbox) -> usize {
        state
            .rows
            .iter()
            .position(|r| matches!(r, Row::Checkbox { knob: k, .. } if *k == knob))
            .unwrap()
    }

    #[test]
    fn test_arch_toggle_is_delta_only() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());

        state.cursor = rule_index(&state, "docker-release");
        state.expand_current();

        // Activating an arch row flips that arch and nothing else
        let toggle = |state: &mut EditorState, arch| {
            state.cursor = checkbox_index(state, Checkbox::Arch(arch));
            state.activate_current();
        };
        toggle(&mut state, DockerPlatform::LinuxArm64);
        toggle(&mut state, DockerPlatform::LinuxAmd64);
        assert_eq!(state.config.docker_release.sync_readme, None);
        // Stored in ALL order regardless of toggle order
        assert_eq!(
            state.config.docker_release.platforms,
            Some(vec![DockerPlatform::LinuxAmd64, DockerPlatform::LinuxArm64])
        );
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("LinuxArm64"), "{ron_str}");
        assert!(state.yaml_preview.contains("buildx"), "{}", state.yaml_preview);

        // Unselecting everything removes the key entirely
        toggle(&mut state, DockerPlatform::LinuxArm64);
        toggle(&mut state, DockerPlatform::LinuxAmd64);
        assert!(state.config.is_default());
    }

    #[test]
    fn test_image_name_editing() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());
        let default = state.default_image();

        state.cursor = rule_index(&state, "docker-build");
        state.expand_current();
        state.cursor = rule_index(&state, "docker-build") + 1;

        // Enter starts editing prefilled with the effective value
        state.activate_current();
        assert_eq!(state.editing(state.cursor), Some(default.as_str()));
        // ...and only on that row
        assert_eq!(state.editing(state.cursor + 1), None);

        // Esc cancels without touching the config
        state.cancel_input();
        assert!(state.config.is_default());

        // Typing a custom name stores the override
        state.activate_current();
        clear_input(&mut state);
        for c in "fossable/cibox".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.docker_build.image_name.as_deref(),
            Some("fossable/cibox")
        );
        assert!(state.yaml_preview.contains("fossable/cibox"));

        // Committing the facts default clears the override
        state.cursor = rule_index(&state, "docker-build") + 1;
        state.activate_current();
        clear_input(&mut state);
        for c in default.chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert!(state.config.is_default());
    }

    #[test]
    fn test_typed_image_name_is_coerced_to_a_valid_reference() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());

        state.cursor = rule_index(&state, "docker-build");
        state.expand_current();
        state.cursor = rule_index(&state, "docker-build") + 1;
        state.activate_current();
        clear_input(&mut state);
        for c in "Owner/App; id".chars() {
            state.input_push(c);
        }
        state.commit_input();

        assert_eq!(
            state.config.docker_build.image_name.as_deref(),
            Some("owner/app-id")
        );
        // What the editor saves has to be something it can read back
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        crate::config::parse_config(&ron_str).unwrap();
        assert!(!state.yaml_preview.contains("; id"), "{}", state.yaml_preview);
    }
}
