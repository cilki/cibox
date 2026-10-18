use crate::config::{serialize_config, target_platforms, CiboxConfig, DockerPlatform};
use crate::detection::ProjectFacts;
use crate::editor::ui::PreviewDiff;
use crate::error::Result;
use crate::generator::JobFilter;
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
    /// One of the string knobs, with the value it currently shows
    TextKnob {
        knob: TextKnob,
        /// The cibox.ron override, if any
        override_value: Option<String>,
        /// The value in effect (facts-derived default when not overridden)
        effective: String,
    },
    /// A checkbox for one of the boolean knobs
    Checkbox {
        knob: Checkbox,
        value: bool,
    },
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

/// A string knob the checklist renders as an editable row. Naming the knob
/// is the whole of what the row has to say about itself: its label and
/// description come from here, and committing an edit dispatches on it
/// directly instead of re-deriving the knob from a (rule id, label) pair —
/// which also keeps [`EditorState::set_knob`] exhaustive, so a knob added to
/// a rule that already has one cannot be silently routed to the wrong field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextKnob {
    /// The docker rules' `image_name`. One knob behind two rows: setting it
    /// on either rule applies to both, so all the row records is which
    /// field an edit writes to.
    ImageName { release: bool },
    /// aur-release's `packages`
    AurPackages,
    /// android-build's `dir`
    AndroidDir,
    /// android-build's `apt_packages`
    AndroidAptPackages,
    /// android-build's `rust_toolchain`
    AndroidToolchain,
}

impl TextKnob {
    /// The cibox.ron field name, shown as the row's label
    pub fn label(&self) -> &'static str {
        match self {
            TextKnob::ImageName { .. } => "image_name",
            TextKnob::AurPackages => "packages",
            TextKnob::AndroidDir => "dir",
            TextKnob::AndroidAptPackages => "apt_packages",
            TextKnob::AndroidToolchain => "rust_toolchain",
        }
    }

    pub fn description(&self) -> &'static str {
        match self {
            TextKnob::ImageName { .. } => {
                "Image name, e.g. \"fossable/cibox\"; applies to both docker rules. Press \
                 Enter to edit."
            }
            TextKnob::AurPackages => {
                "Comma-separated AUR package names, one release job each; the default is \
                 named after the git remote. Press Enter to edit."
            }
            TextKnob::AndroidDir => {
                "Gradle project directory relative to the repository root; the default is \
                 the detected one. Press Enter to edit."
            }
            TextKnob::AndroidAptPackages => {
                "Comma-separated Debian packages the native build needs on the host, e.g. \
                 \"libasound2-dev, llvm\". Press Enter to edit."
            }
            TextKnob::AndroidToolchain => {
                "Rust toolchain for the cargo-ndk pre-build: \"stable\", \"nightly\", or \
                 \"x.y[.z]\". Press Enter to edit."
            }
        }
    }
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

    /// The platforms a plain `cibox update` would target: every one with a
    /// cibox-managed file on disk, or the inferred one when there is none
    /// yet. Re-derived on every refresh, since an update run elsewhere can
    /// create a file.
    pub platforms: Vec<Platform>,

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
    /// Which panel the arrow keys act on
    pub focus: Focus,
    /// Whether the keybindings dialog (opened with `?`) is showing
    pub help_open: bool,
    /// What the next `cibox update` would change in the pipeline files on
    /// disk, kept here so a redraw never has to compute it again. Empty when
    /// there is nothing to show, which hides the panel.
    pub preview: PreviewDiff,
    /// Rows the preview is scrolled down by
    pub preview_scroll: u16,
    /// Inner (width, height) of the preview panel as last drawn, which
    /// paging and the scroll limit are measured against; zero while hidden
    pub preview_viewport: (u16, u16),

    // Exit flags
    pub should_quit: bool,
}

/// The panel keyboard navigation goes to
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Rules,
    Preview,
}

impl EditorState {
    pub fn new(facts: ProjectFacts, working_dir: PathBuf) -> Result<Self> {
        let ron_path = working_dir.join("cibox.ron");
        let (config, has_ron_file) = match std::fs::read_to_string(&ron_path) {
            Ok(ron_str) => (crate::config::parse_config(&ron_str)?, true),
            Err(_) => (CiboxConfig::default(), false),
        };

        let mut state = Self {
            facts,
            working_dir,
            config,
            has_ron_file,
            platforms: Vec::new(),
            rows: Vec::new(),
            cursor: 0,
            expanded: HashSet::new(),
            input: None,
            focus: Focus::Rules,
            help_open: false,
            preview: PreviewDiff::default(),
            preview_scroll: 0,
            preview_viewport: (0, 0),
            should_quit: false,
        };

        state.refresh();
        Ok(state)
    }

    /// The image name detection would use without any override
    fn default_image(&self) -> String {
        crate::rules::docker_image(&self.facts, &CiboxConfig::default())
    }

    /// The AUR package list detection would use without any override
    fn default_aur_packages(&self) -> Vec<String> {
        crate::rules::aur_packages(&self.facts, &CiboxConfig::default())
    }

    /// The Gradle project directory detection found, `.` when there is none
    fn default_android_dir(&self) -> String {
        self.facts
            .android
            .as_ref()
            .map(|a| a.dir.clone())
            .unwrap_or_else(|| ".".to_string())
    }

    /// What one text knob's row shows: the cibox.ron override, if any, and
    /// the value in effect — the facts-derived default when there is none.
    fn knob_values(&self, knob: TextKnob) -> (Option<String>, String) {
        let joined = |list: &Option<Vec<String>>| list.as_ref().map(|items| items.join(", "));
        // An override is the effective value for every knob but image_name,
        // which is shared: a name set on either docker rule feeds both rows
        let from_override = |override_value: Option<String>, default: String| {
            let effective = override_value.clone().unwrap_or(default);
            (override_value, effective)
        };
        let android = &self.config.android_build;
        match knob {
            TextKnob::ImageName { release } => (
                if release {
                    self.config.docker_release.image_name.clone()
                } else {
                    self.config.docker_build.image_name.clone()
                },
                crate::rules::docker_image(&self.facts, &self.config),
            ),
            TextKnob::AurPackages => (
                joined(&self.config.aur_release.packages),
                crate::rules::aur_packages(&self.facts, &self.config).join(", "),
            ),
            TextKnob::AndroidDir => from_override(android.dir.clone(), self.default_android_dir()),
            TextKnob::AndroidAptPackages => {
                from_override(joined(&android.apt_packages), String::new())
            }
            TextKnob::AndroidToolchain => from_override(
                android.rust_toolchain.clone(),
                crate::config::android::DEFAULT_TOOLCHAIN.to_string(),
            ),
        }
    }

    /// Store the value committed into one text knob. The values are written
    /// to cibox.ron and from there into shell commands, so each is coerced
    /// (or, where there is no sensible coercion, dropped) into something the
    /// config parser accepts back. A value matching the facts-derived
    /// default — or an empty one — removes the override, so cibox.ron stays
    /// delta-only.
    fn set_knob(&mut self, knob: TextKnob, value: &str) {
        match knob {
            TextKnob::ImageName { release } => {
                let override_value = (!value.is_empty())
                    .then(|| crate::config::image::coerce_reference(value))
                    .filter(|value| *value != self.default_image());
                if release {
                    self.config.docker_release.image_name = override_value;
                } else {
                    self.config.docker_build.image_name = override_value;
                }
            }
            TextKnob::AurPackages => {
                let packages = comma_list(value, |name| {
                    Some(crate::config::aur::coerce_package_name(name))
                });
                self.config.aur_release.packages = (!packages.is_empty())
                    .then_some(packages)
                    .filter(|packages| *packages != self.default_aur_packages());
            }
            // A path cannot be coerced into a different one that still
            // exists, so an invalid dir clears the override instead
            TextKnob::AndroidDir => {
                self.config.android_build.dir = Some(value.to_string())
                    .filter(|v| !v.is_empty())
                    .filter(|v| crate::config::android::is_valid_project_dir(v))
                    .filter(|v| *v != self.default_android_dir());
            }
            TextKnob::AndroidAptPackages => {
                let packages = comma_list(value, |name| {
                    let name = name.to_ascii_lowercase();
                    crate::config::android::is_valid_apt_package(&name).then_some(name)
                });
                self.config.android_build.apt_packages = (!packages.is_empty()).then_some(packages);
            }
            TextKnob::AndroidToolchain => {
                self.config.android_build.rust_toolchain = Some(value.to_string())
                    .filter(|v| !v.is_empty())
                    .filter(|v| crate::config::android::is_valid_toolchain(v))
                    .filter(|v| v != crate::config::android::DEFAULT_TOOLCHAIN);
            }
        }
    }

    fn text_knob_row(&self, knob: TextKnob) -> Row {
        let (override_value, effective) = self.knob_values(knob);
        Row::TextKnob {
            knob,
            override_value,
            effective,
        }
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

        match rule_id {
            "aur-release" => vec![self.text_knob_row(TextKnob::AurPackages)],
            "android-build" => [
                TextKnob::AndroidDir,
                TextKnob::AndroidAptPackages,
                TextKnob::AndroidToolchain,
            ]
            .into_iter()
            .map(|knob| self.text_knob_row(knob))
            .collect(),
            "docker-build" => vec![self.text_knob_row(TextKnob::ImageName { release: false })],
            "docker-release" => {
                let mut rows = vec![
                    self.text_knob_row(TextKnob::ImageName { release: true }),
                    Row::Checkbox {
                        knob: Checkbox::SyncReadme,
                        value: self.config.docker_release.sync_readme.unwrap_or(false),
                    },
                ];
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
                rows
            }
            _ => Vec::new(),
        }
    }

    /// Re-resolve rules against the current config and recompute what
    /// `cibox update` would change. The pipeline files are re-read every time,
    /// so an update run from another terminal shows up on the next edit.
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

        // Exactly what a plain `cibox update` would decide: the platforms it
        // would pick, every rule selected, merging rather than forcing
        self.facts.managed_ci = crate::detection::managed_ci(&self.working_dir);
        self.platforms = target_platforms(&self.facts, &[]).0;
        let filter = JobFilter {
            resolved: &resolved,
            selection: None,
        };
        let pending =
            crate::generator::plan_all(&self.facts, &resolved, &self.platforms).and_then(|plans| {
                crate::generator::pending_updates(&self.working_dir, &plans, &filter, false)
            });
        // The rule each job key in the files belongs to, so the preview can
        // tell which lines are whose
        let owner = |key: &str| {
            resolved
                .iter()
                .find(|r| r.rule.owns_job_id(key))
                .map(|r| r.rule.id())
        };
        self.preview = match pending {
            Ok(updates) => PreviewDiff::from_updates(&updates, &owner),
            // e.g. a pipeline file that isn't valid YAML: update would refuse
            // too, and the reason is what the user needs to see
            Err(e) => PreviewDiff::error(&format!("{e:#}")),
        };
        // Land on what the edit just changed, if the selected rule's job is
        // part of it, and at the top otherwise
        self.preview_scroll = 0;
        self.follow_cursor();
        if self.preview.is_empty() {
            self.focus = Focus::Rules;
        }
    }

    /// The rule the row under the cursor belongs to: the rule itself, or the
    /// rule a config-option row sits under
    pub fn selected_rule(&self) -> Option<&'static str> {
        match self.rows.get(self.parent_rule_index(self.cursor)?)? {
            Row::Rule(rule) => Some(rule.id),
            _ => None,
        }
    }

    /// Scroll the preview so the selected rule's job is at the top. Left
    /// where it is when the job isn't in the diff — update leaves it alone,
    /// so there is nothing of it to show.
    pub fn follow_cursor(&mut self) {
        let Some(line) = self
            .selected_rule()
            .and_then(|rule| self.preview.anchor(rule))
        else {
            return;
        };
        let (width, height) = self.preview_viewport;
        self.preview_scroll = if width == 0 {
            // Not drawn yet, so there is no width to wrap at: count lines,
            // and let `set_preview_viewport` correct it once there is
            u16::try_from(line).unwrap_or(u16::MAX)
        } else {
            self.preview
                .rows_before(line, width)
                .min(self.preview.max_scroll(width, height))
        };
    }

    /// Move the rules cursor by `delta` rows, within the list, taking the
    /// preview along to the job of the rule it lands on
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1);
        self.cursor = self.cursor.saturating_add_signed(delta).min(last);
        self.follow_cursor();
    }

    /// Record the inner size of the preview panel as drawn. A new size
    /// rewraps the diff, which leaves a scroll offset counted in rows of the
    /// old one pointing somewhere else, so the preview re-follows the cursor.
    pub fn set_preview_viewport(&mut self, viewport: (u16, u16)) {
        if viewport != self.preview_viewport {
            self.preview_viewport = viewport;
            self.follow_cursor();
        }
    }

    /// Whether there is anything for the preview panel to show
    pub fn preview_shown(&self) -> bool {
        !self.preview.is_empty()
    }

    /// Tab: move between the rules and the preview, which can only take
    /// focus while it is shown
    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Rules if self.preview_shown() => Focus::Preview,
            _ => Focus::Rules,
        };
    }

    /// Scroll the preview by `rows`, up when negative, never past its end
    pub fn scroll_preview(&mut self, rows: i32) {
        let (width, height) = self.preview_viewport;
        let max = self.preview.max_scroll(width, height);
        let scroll = i32::from(self.preview_scroll).saturating_add(rows);
        self.preview_scroll = scroll.clamp(0, i32::from(max)) as u16;
    }

    /// One screenful of the preview, keeping a line of overlap
    pub fn preview_page(&self) -> i32 {
        i32::from(self.preview_viewport.1.saturating_sub(1).max(1))
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
        self.config.set_enabled_override(rule.id, override_value);
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

    /// Commit the text being typed into the knob row under the cursor
    pub fn commit_input(&mut self) {
        let Some(buffer) = self.input.take() else {
            return;
        };
        let value = buffer.trim().to_string();
        match self.rows.get(self.cursor).cloned() {
            Some(Row::TextKnob { knob, .. }) => self.set_knob(knob, &value),
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
        }
    }

    /// What the information bar says about the row under the cursor. Derived
    /// on demand: cached in a field it would have to be refreshed from every
    /// cursor move and every rebuild of the rows, and goes stale the moment
    /// one of them forgets.
    pub fn current_description(&self) -> String {
        match self.current_row() {
            Some(Row::Rule(row)) => row.description.to_string(),
            Some(Row::TextKnob { knob, .. }) => knob.description().to_string(),
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
        }
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

/// Read a comma-separated knob value as a list: each entry goes through
/// `entry`, which returns the value to store or `None` to drop it, and the
/// result is normalized exactly as `rules::resolve` would — empties dropped,
/// duplicates collapsed — so what the preview shows is what the saved file
/// means.
fn comma_list(value: &str, entry: impl Fn(&str) -> Option<String>) -> Vec<String> {
    let entries: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|raw| !raw.is_empty())
        .filter_map(entry)
        .collect();
    clean_versions(Some(&entries))
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
        EditorState::new(facts, dir.to_path_buf()).unwrap()
    }

    /// Every file the state's rules generate, with path banners — what the
    /// rules emit, independent of whether a pipeline exists on disk to diff
    fn generated(state: &EditorState) -> String {
        let resolved = resolve(&state.facts, &state.config);
        let mut out = String::new();
        for &platform in &state.platforms {
            match crate::generator::generate(&state.facts, &resolved, platform) {
                Ok(outputs) => {
                    for (path, content) in outputs {
                        out.push_str(&format!("# ==> {} <==\n{content}", path.display()));
                    }
                }
                Err(e) => out.push_str(&format!("# {e}\n")),
            }
        }
        out
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
        assert!(generated(&state).contains("cargo test"));
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
    fn test_rules_generate_for_multiple_environments() {
        let dir = docker_dir();
        let state = state_for(dir.path());
        // GitHub splits the jobs over ci.yml and release.yml
        assert!(
            generated(&state).contains("cargo test"),
            "{}",
            generated(&state)
        );
        assert!(
            generated(&state).contains("docker build"),
            "{}",
            generated(&state)
        );
        assert!(
            generated(&state).contains("release.yml"),
            "{}",
            generated(&state)
        );
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
            Row::TextKnob {
                knob: TextKnob::ImageName { release: true },
                ..
            }
        ));
        assert!(matches!(
            state.rows[idx + 2],
            Row::Checkbox {
                knob: Checkbox::SyncReadme,
                ..
            }
        ));
        // image_name + sync_readme + one row per DockerPlatform
        assert_eq!(
            state.rows.len(),
            collapsed_len + 2 + DockerPlatform::ALL.len()
        );

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
        assert!(
            generated(&state).contains("docker-pushrm"),
            "{}",
            generated(&state)
        );

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
        assert!(matches!(
            state.rows[idx + 1],
            Row::Version { index: None, .. }
        ));

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
        assert!(
            generated(&state).contains("matrix"),
            "{}",
            generated(&state)
        );

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
        assert!(
            generated(&state).contains("buildx"),
            "{}",
            generated(&state)
        );

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
        assert!(generated(&state).contains("fossable/cibox"));

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
    fn test_aur_packages_editing() {
        let dir = rust_dir();
        let mut state = state_for(dir.path());

        // Off by default but still expandable for configuration
        assert!(!rule_row(&state, "aur-release").enabled);
        state.cursor = rule_index(&state, "aur-release");
        state.activate_current();
        state.expand_current();
        let idx = rule_index(&state, "aur-release");
        assert!(matches!(
            state.rows[idx + 1],
            Row::TextKnob {
                knob: TextKnob::AurPackages,
                ..
            }
        ));

        // Typed names are coerced, split on commas, and deduped so the saved
        // file is something the config parser accepts back
        state.cursor = idx + 1;
        state.activate_current();
        clear_input(&mut state);
        for c in "foo, Bar baz, foo,".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.aur_release.packages,
            Some(vec!["foo".to_string(), "bar-baz".to_string()])
        );
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        crate::config::parse_config(&ron_str).unwrap();
        assert!(
            generated(&state).contains("aur-release-foo"),
            "{}",
            generated(&state)
        );

        // Committing the facts default (or nothing) clears the override
        let default = state.default_aur_packages().join(", ");
        state.cursor = rule_index(&state, "aur-release") + 1;
        state.activate_current();
        clear_input(&mut state);
        for c in default.chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(state.config.aur_release.packages, None);
    }

    fn android_dir() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let gradle = dir.path().join("mobile/android");
        fs::create_dir_all(gradle.join("app/src/main")).unwrap();
        fs::write(gradle.join("gradlew"), "").unwrap();
        fs::write(gradle.join("settings.gradle"), "").unwrap();
        fs::write(gradle.join("app/src/main/AndroidManifest.xml"), "").unwrap();
        dir
    }

    #[test]
    fn test_android_knob_editing() {
        let dir = android_dir();
        let mut state = state_for(dir.path());

        assert!(rule_row(&state, "android-build").enabled);
        state.cursor = rule_index(&state, "android-build");
        state.expand_current();
        let idx = rule_index(&state, "android-build");
        for (offset, knob) in [
            TextKnob::AndroidDir,
            TextKnob::AndroidAptPackages,
            TextKnob::AndroidToolchain,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(
                matches!(state.rows[idx + 1 + offset], Row::TextKnob { knob: k, .. } if k == knob),
                "{:?}",
                state.rows[idx + 1 + offset]
            );
        }

        // The dir prefills with the detected value, and committing it back
        // stores no override
        state.cursor = idx + 1;
        state.activate_current();
        assert_eq!(state.editing(state.cursor), Some("mobile/android"));
        state.commit_input();
        assert_eq!(state.config.android_build.dir, None);

        // An invalid dir clears the override rather than saving something
        // the config parser would reject
        state.activate_current();
        clear_input(&mut state);
        for c in "x; curl evil.sh".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(state.config.android_build.dir, None);

        // Typed packages are normalized, filtered, and deduped
        state.cursor = rule_index(&state, "android-build") + 2;
        state.activate_current();
        clear_input(&mut state);
        for c in "LLVM, libasound2-dev, pkg$(id), llvm,".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.android_build.apt_packages,
            Some(vec!["llvm".to_string(), "libasound2-dev".to_string()])
        );

        // The toolchain accepts the known shapes and drops the default
        state.cursor = rule_index(&state, "android-build") + 3;
        state.activate_current();
        clear_input(&mut state);
        for c in "nightly".chars() {
            state.input_push(c);
        }
        state.commit_input();
        assert_eq!(
            state.config.android_build.rust_toolchain.as_deref(),
            Some("nightly")
        );

        // What the editor saves has to be something it can read back
        let ron_str = fs::read_to_string(dir.path().join("cibox.ron")).unwrap();
        crate::config::parse_config(&ron_str).unwrap();
        assert!(
            generated(&state).contains("android-build"),
            "{}",
            generated(&state)
        );
    }

    /// Committing the value a knob row already shows has to leave the config
    /// alone: what the row reads and what a commit writes must be the same
    /// knob. A row wired to another knob's field would instead save an
    /// override of that knob's default — and cibox.ron would stop being a
    /// delta.
    #[test]
    fn test_committing_the_shown_value_stores_no_override() {
        let dir = android_dir();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let mut state = state_for(dir.path());

        for id in [
            "docker-build",
            "docker-release",
            "aur-release",
            "android-build",
        ] {
            state.cursor = rule_index(&state, id);
            state.expand_current();
        }
        let text_knobs: Vec<usize> = state
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches!(row, Row::TextKnob { .. }))
            .map(|(index, _)| index)
            .collect();
        // image_name twice (once per docker rule), packages, and the three
        // android knobs
        assert_eq!(text_knobs.len(), 6, "a text knob has no row to edit");

        for index in text_knobs {
            state.cursor = index;
            // Enter prefills the buffer with the effective value; committing
            // it unchanged is the user saying "leave it as it is"
            state.activate_current();
            state.commit_input();
            assert!(
                state.config.is_default(),
                "{:?} saved an override of its own default",
                state.rows[index]
            );
        }
    }

    /// Each knob row writes to its own field and to nothing else, including
    /// the two rows of the shared image_name knob, which write to different
    /// rules
    #[test]
    fn test_each_knob_row_writes_only_its_own_field() {
        let dir = android_dir();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let mut state = state_for(dir.path());

        for id in [
            "docker-build",
            "docker-release",
            "aur-release",
            "android-build",
        ] {
            state.cursor = rule_index(&state, id);
            state.expand_current();
        }
        let rows: Vec<(usize, TextKnob)> = state
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| match row {
                Row::TextKnob { knob, .. } => Some((index, *knob)),
                _ => None,
            })
            .collect();
        assert_eq!(rows.len(), 6, "a text knob has no row to edit");

        for (index, knob) in rows {
            let typed = match knob {
                TextKnob::ImageName { release: false } => "owner/build",
                TextKnob::ImageName { release: true } => "owner/release",
                TextKnob::AurPackages => "pkgx",
                TextKnob::AndroidDir => "other/dir",
                TextKnob::AndroidAptPackages => "llvm",
                TextKnob::AndroidToolchain => "nightly",
            };
            state.cursor = index;
            state.activate_current();
            clear_input(&mut state);
            for c in typed.chars() {
                state.input_push(c);
            }
            state.commit_input();
        }

        assert_eq!(
            state.config.docker_build.image_name.as_deref(),
            Some("owner/build")
        );
        assert_eq!(
            state.config.docker_release.image_name.as_deref(),
            Some("owner/release")
        );
        assert_eq!(
            state.config.aur_release.packages,
            Some(vec!["pkgx".to_string()])
        );
        let android = &state.config.android_build;
        assert_eq!(android.dir.as_deref(), Some("other/dir"));
        assert_eq!(android.apt_packages, Some(vec!["llvm".to_string()]));
        assert_eq!(android.rust_toolchain.as_deref(), Some("nightly"));
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
        assert!(!generated(&state).contains("; id"), "{}", generated(&state));
    }
}
