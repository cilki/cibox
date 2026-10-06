use crate::config::{infer_platform, serialize_config, CiboxConfig, DockerPlatform};
use crate::detection::ProjectFacts;
use crate::error::Result;
use crate::rules::resolve;
use std::collections::HashSet;
use std::path::PathBuf;

pub use crate::config::Platform;

/// Rules whose config options can be expanded inline
const EXPANDABLE_RULES: [&str; 6] = [
    "docker-build",
    "docker-release",
    "rust-test",
    "python-test",
    "go-test",
    "node-test",
];

/// Test rules with a versions knob, and the default image shown when unset
const VERSIONED_RULES: [(&str, &str); 4] = [
    ("rust-test", "rust:latest"),
    ("python-test", "python:3.12"),
    ("go-test", "golang:1.23"),
    ("node-test", "node:22"),
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
    /// One target-platform checkbox under docker-release
    ArchOption { arch: DockerPlatform, selected: bool },
    /// One configured toolchain version under an expanded test rule
    VersionItem {
        rule_id: &'static str,
        index: usize,
        value: String,
    },
    /// Trailing "add a version" action row under an expanded test rule
    AddVersion {
        rule_id: &'static str,
        default_image: &'static str,
    },
}

/// Which knob a text input edits
#[derive(Debug, Clone, PartialEq)]
pub enum KnobTarget {
    ImageName,
    /// Some(i) edits versions[i]; None appends a new version
    Version(Option<usize>),
}

/// In-progress edit of a text knob
#[derive(Debug, Clone)]
pub struct KnobInput {
    pub rule_id: &'static str,
    pub target: KnobTarget,
    pub buffer: String,
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
    /// Text knob currently being edited, if any
    pub input: Option<KnobInput>,

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
        self.facts
            .repo_slug
            .clone()
            .unwrap_or_else(|| self.facts.dir_name.clone())
    }

    /// The image name in effect, mirroring the precedence in rules::resolve
    fn effective_image(&self) -> String {
        self.config
            .docker_build
            .image_name
            .clone()
            .or_else(|| self.config.docker_release.image_name.clone())
            .unwrap_or_else(|| self.default_image())
    }

    /// Re-resolve rules against the current config and regenerate the preview
    pub fn refresh(&mut self) {
        let resolved = resolve(&self.facts, &self.config);
        self.rows = Vec::new();
        for r in &resolved {
            let id = r.rule.id();
            let expandable = EXPANDABLE_RULES.contains(&id);
            let expanded = expandable && self.expanded.contains(id);
            self.rows.push(Row::Rule(RuleRow {
                id,
                name: r.rule.name(),
                description: r.rule.description(),
                detected: r.detected,
                enabled: r.enabled,
                expandable,
                expanded,
            }));
            if expanded {
                if let Some((_, default_image)) =
                    VERSIONED_RULES.iter().find(|(vid, _)| *vid == id)
                {
                    let versions = self
                        .config
                        .versions_override(id)
                        .cloned()
                        .unwrap_or_default();
                    for (index, value) in versions.iter().enumerate() {
                        self.rows.push(Row::VersionItem {
                            rule_id: id,
                            index,
                            value: value.clone(),
                        });
                    }
                    self.rows.push(Row::AddVersion {
                        rule_id: id,
                        default_image,
                    });
                } else {
                    let override_value = match id {
                        "docker-build" => self.config.docker_build.image_name.clone(),
                        _ => self.config.docker_release.image_name.clone(),
                    };
                    self.rows.push(Row::TextKnob {
                        rule_id: id,
                        label: "image_name",
                        description: "Image name, e.g. \"fossable/cibox\"; applies to both docker \
                                      rules. Press Enter to edit.",
                        override_value,
                        effective: self.effective_image(),
                    });
                    if id == "docker-release" {
                        let selected = self
                            .config
                            .docker_release
                            .platforms
                            .clone()
                            .unwrap_or_default();
                        for arch in DockerPlatform::ALL {
                            self.rows.push(Row::ArchOption {
                                arch,
                                selected: selected.contains(&arch),
                            });
                        }
                    }
                }
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

    /// Activate the row under the cursor: toggle a rule or arch checkbox, or
    /// start editing a text knob
    pub fn activate_current(&mut self) {
        match self.rows.get(self.cursor).cloned() {
            Some(Row::Rule(rule)) => self.toggle_rule(&rule),
            Some(Row::TextKnob {
                rule_id, effective, ..
            }) => {
                self.input = Some(KnobInput {
                    rule_id,
                    target: KnobTarget::ImageName,
                    buffer: effective,
                });
            }
            Some(Row::ArchOption { arch, .. }) => self.toggle_arch(arch),
            Some(Row::VersionItem {
                rule_id,
                index,
                value,
            }) => {
                self.input = Some(KnobInput {
                    rule_id,
                    target: KnobTarget::Version(Some(index)),
                    buffer: value,
                });
            }
            Some(Row::AddVersion { rule_id, .. }) => {
                self.input = Some(KnobInput {
                    rule_id,
                    target: KnobTarget::Version(None),
                    buffer: String::new(),
                });
            }
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

    /// Flip one docker-release target platform; an empty selection collapses
    /// back to "unset" so cibox.ron stays delta-only
    pub fn toggle_arch(&mut self, arch: DockerPlatform) {
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
        let selected: Vec<DockerPlatform> = DockerPlatform::ALL
            .iter()
            .copied()
            .filter(|p| selected.contains(p))
            .collect();
        self.config.docker_release.platforms = (!selected.is_empty()).then_some(selected);
        self.refresh();
        self.auto_save_ron();
    }

    /// Commit the text knob being edited. A value matching the facts-derived
    /// default (or an empty one) removes the override.
    pub fn commit_input(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let value = input.buffer.trim().to_string();
        match input.target {
            KnobTarget::ImageName => {
                let override_value =
                    (!value.is_empty() && value != self.default_image()).then_some(value);
                match input.rule_id {
                    "docker-build" => self.config.docker_build.image_name = override_value,
                    "docker-release" => self.config.docker_release.image_name = override_value,
                    _ => {}
                }
            }
            KnobTarget::Version(slot) => {
                let mut versions = self
                    .config
                    .versions_override(input.rule_id)
                    .cloned()
                    .unwrap_or_default();
                match slot {
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
                let mut seen: Vec<String> = Vec::new();
                versions.retain(|v| {
                    if seen.contains(v) {
                        false
                    } else {
                        seen.push(v.clone());
                        true
                    }
                });
                self.config
                    .set_versions_override(input.rule_id, (!versions.is_empty()).then_some(versions));
            }
        }
        self.refresh();
        self.update_current_item_description();
        self.auto_save_ron();
    }

    /// Remove the toolchain version under the cursor; an empty list collapses
    /// back to "unset" so cibox.ron stays delta-only
    pub fn delete_current_version(&mut self) {
        let Some(Row::VersionItem { rule_id, index, .. }) = self.rows.get(self.cursor).cloned()
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
        if let Some(input) = &mut self.input {
            input.buffer.push(c);
        }
    }

    pub fn input_backspace(&mut self) {
        if let Some(input) = &mut self.input {
            input.buffer.pop();
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
            Some(Row::ArchOption { arch, .. }) => format!(
                "Include {} in the released multi-arch image",
                arch.as_str()
            ),
            Some(Row::VersionItem { .. }) => {
                "Toolchain version run as one matrix leg; Enter to edit, d to remove".to_string()
            }
            Some(Row::AddVersion { default_image, .. }) => format!(
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
        // image_name + one row per DockerPlatform
        assert_eq!(state.rows.len(), collapsed_len + 1 + DockerPlatform::ALL.len());

        // Collapsing from a child row jumps back to the parent
        state.cursor = idx + 2;
        state.collapse_current();
        assert_eq!(state.cursor, rule_index(&state, "docker-release"));
        assert_eq!(state.rows.len(), collapsed_len);
    }

    #[test]
    fn test_version_list_editing() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());

        // Expanding a versioned test rule shows only the add row
        state.cursor = rule_index(&state, "rust-test");
        state.expand_current();
        let idx = rule_index(&state, "rust-test");
        assert!(matches!(state.rows[idx + 1], Row::AddVersion { .. }));

        // Adding a version through the add row
        state.cursor = idx + 1;
        state.activate_current();
        assert_eq!(
            state.input.as_ref().unwrap().target,
            KnobTarget::Version(None)
        );
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
            Row::VersionItem { value, .. } if value == "1.85"
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
        let buffer_len = state.input.as_ref().unwrap().buffer.len();
        for _ in 0..buffer_len {
            state.input_backspace();
        }
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

    #[test]
    fn test_delete_only_acts_on_version_rows() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());
        state.cursor = rule_index(&state, "rust-test");
        state.delete_current_version();
        assert!(state.config.is_default());
    }

    #[test]
    fn test_arch_toggle_is_delta_only() {
        let dir = docker_dir();
        let mut state = state_for(dir.path());

        state.cursor = rule_index(&state, "docker-release");
        state.expand_current();

        state.toggle_arch(DockerPlatform::LinuxArm64);
        state.toggle_arch(DockerPlatform::LinuxAmd64);
        // Stored in ALL order regardless of toggle order
        assert_eq!(
            state.config.docker_release.platforms,
            Some(vec![DockerPlatform::LinuxAmd64, DockerPlatform::LinuxArm64])
        );
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        assert!(ron_str.contains("LinuxArm64"), "{ron_str}");
        assert!(state.yaml_preview.contains("buildx"), "{}", state.yaml_preview);

        // Unselecting everything removes the key entirely
        state.toggle_arch(DockerPlatform::LinuxArm64);
        state.toggle_arch(DockerPlatform::LinuxAmd64);
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
        assert_eq!(state.input.as_ref().unwrap().buffer, default);

        // Esc cancels without touching the config
        state.cancel_input();
        assert!(state.config.is_default());

        // Typing a custom name stores the override
        state.activate_current();
        for _ in 0..state.input.as_ref().unwrap().buffer.len() {
            state.input_backspace();
        }
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
        let buffer_len = state.input.as_ref().unwrap().buffer.len();
        for _ in 0..buffer_len {
            state.input_backspace();
        }
        for c in default.chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert!(state.config.is_default());
    }
}
