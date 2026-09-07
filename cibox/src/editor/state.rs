use crate::detection::{DetectionResult, ProjectType};
use crate::editor::config::{OptionValue, PresetConfig};
use crate::editor::registry::{build_registry, PresetRegistry};
use crate::error::Result;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

pub use crate::config::Platform;

/// Fixed category display order
const CATEGORY_ORDER: &[&str] = &["Languages", "Packaging", "Documentation", "Security"];

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TreeItem {
    Category(String),           // category name
    Preset(String),             // preset_id
    Field(String, String),      // (preset_id, field_id)
}

/// One CI pipeline being edited: a target platform and its own preset settings
pub struct PipelineState {
    pub platform: Platform,
    pub preset_configs: HashMap<String, PresetConfig>,
    /// Whether this pipeline is saved to cibox.ron and written as a CI file.
    /// Pipelines created by browsing platforms stay transient until modified.
    pub persist: bool,
}

pub struct EditorState {
    // Project context
    pub project_type: ProjectType,
    pub language_version: String,
    pub working_dir: PathBuf,

    // Pipelines being edited, one per platform
    pub pipelines: Vec<PipelineState>,
    pub active_pipeline: usize,

    // Dynamic preset configuration
    pub registry: Arc<PresetRegistry>,

    // UI state - tree structure
    pub expanded_categories: HashSet<String>,
    pub expanded_presets: HashSet<String>,
    pub tree_items: Vec<TreeItem>,
    pub tree_cursor: usize,
    pub platform_menu_open: bool,
    pub platform_menu_cursor: usize,

    // Preview scroll state
    pub preview_scroll: u16,

    // Generated output
    pub yaml_preview: String,
    pub generation_error: Option<String>,

    // Existing YAML for diff
    pub existing_yaml: Option<String>,

    // UI info
    pub current_item_description: String,

    // Exit flags
    pub should_quit: bool,
    pub should_write: bool,
}

impl EditorState {
    pub fn from_detection(
        detection: DetectionResult,
        platform: Option<String>,
        working_dir: PathBuf,
    ) -> Result<Self> {
        use std::str::FromStr;

        let project_type = detection.project_type.clone();
        let language_version = detection
            .language_version
            .clone()
            .unwrap_or_else(|| "stable".to_string());

        let target_platform = if let Some(p) = platform {
            Platform::from_str(&p).map_err(|_| crate::error::unsupported_platform_error(&p))?
        } else {
            Platform::GitHub
        };

        // Build the preset registry
        let registry = Arc::new(build_registry());

        // Initialize preset configs for all presets
        let mut preset_configs = HashMap::new();
        let mut expanded_categories = HashSet::new();
        let mut expanded_presets = HashSet::new();

        for preset in registry.all() {
            let preset_id = preset.preset_id();
            let matches = preset.matches_project(&project_type, &working_dir);

            // Create default config based on whether it matches
            let config = preset.default_config(matches);
            preset_configs.insert(preset_id.to_string(), config);

            // Expand matching presets and their categories by default
            if matches {
                expanded_presets.insert(preset_id.to_string());
                expanded_categories.insert(preset.preset_category().to_string());
            }
        }

        // Try to load existing YAML file
        let output_path = working_dir.join(target_platform.output_path());
        let existing_yaml = std::fs::read_to_string(&output_path).ok();

        let mut state = Self {
            project_type,
            language_version,
            working_dir,
            pipelines: vec![PipelineState {
                platform: target_platform,
                preset_configs,
                persist: true,
            }],
            active_pipeline: 0,
            registry,
            expanded_categories,
            expanded_presets,
            tree_items: Vec::new(),
            tree_cursor: 0,
            platform_menu_open: false,
            platform_menu_cursor: Platform::all()
                .iter()
                .position(|&p| p == target_platform)
                .unwrap_or(0),
            preview_scroll: 0,
            yaml_preview: String::new(),
            generation_error: None,
            existing_yaml,
            current_item_description: String::new(),
            should_quit: false,
            should_write: false,
        };

        state.rebuild_tree();
        state.regenerate_yaml();
        state.update_current_item_description();
        Ok(state)
    }

    /// Platform of the pipeline currently being edited
    pub fn target_platform(&self) -> Platform {
        self.pipelines[self.active_pipeline].platform
    }

    /// Preset settings of the pipeline currently being edited
    pub fn preset_configs(&self) -> &HashMap<String, PresetConfig> {
        &self.pipelines[self.active_pipeline].preset_configs
    }

    pub fn preset_configs_mut(&mut self) -> &mut HashMap<String, PresetConfig> {
        &mut self.pipelines[self.active_pipeline].preset_configs
    }

    /// Whether a persisted pipeline exists for the given platform
    pub fn has_pipeline(&self, platform: Platform) -> bool {
        self.pipelines
            .iter()
            .any(|p| p.platform == platform && p.persist)
    }

    /// Number of pipelines that will be saved
    pub fn persisted_pipeline_count(&self) -> usize {
        self.pipelines.iter().filter(|p| p.persist).count()
    }

    fn mark_active_persist(&mut self) {
        self.pipelines[self.active_pipeline].persist = true;
    }

    pub fn rebuild_tree(&mut self) {
        self.tree_items.clear();

        // Get all presets grouped by category
        let all_presets: Vec<_> = self.registry.all().into_iter().collect();

        for &category in CATEGORY_ORDER {
            // Collect presets in this category
            let mut category_presets: Vec<_> = all_presets
                .iter()
                .filter(|p| p.preset_category() == category)
                .collect();

            if category_presets.is_empty() {
                continue;
            }

            // Sort: matching presets first
            category_presets.sort_by_key(|preset| {
                !preset.matches_project(&self.project_type, &self.working_dir)
            });

            self.tree_items.push(TreeItem::Category(category.to_string()));

            if self.expanded_categories.contains(category) {
                for preset in category_presets {
                    let preset_id = preset.preset_id().to_string();
                    self.tree_items.push(TreeItem::Preset(preset_id.clone()));

                    if self.expanded_presets.contains(&preset_id) {
                        for field in preset.fields() {
                            self.tree_items.push(TreeItem::Field(
                                preset_id.clone(),
                                field.id.clone(),
                            ));
                        }
                    }
                }
            }
        }
    }

    pub fn toggle_category_expand(&mut self, category: &str) {
        if self.expanded_categories.contains(category) {
            self.expanded_categories.remove(category);
        } else {
            self.expanded_categories.insert(category.to_string());
        }
        self.rebuild_tree();
    }

    pub fn toggle_preset_expand(&mut self, preset_id: &str) {
        if self.expanded_presets.contains(preset_id) {
            self.expanded_presets.remove(preset_id);
        } else {
            self.expanded_presets.insert(preset_id.to_string());
        }
        self.rebuild_tree();
    }

    pub fn current_item(&self) -> Option<&TreeItem> {
        self.tree_items.get(self.tree_cursor)
    }

    pub fn regenerate_yaml(&mut self) {
        // Reset scroll position when regenerating
        self.preview_scroll = 0;

        // Find the first preset that has any options enabled
        let active_preset = self.registry.all().into_iter().find(|preset| {
            if let Some(config) = self.preset_configs().get(preset.preset_id()) {
                self.has_any_options_enabled(config)
            } else {
                false
            }
        });

        let Some(preset) = active_preset else {
            self.yaml_preview = "# No preset options enabled\n# Enable at least one option to generate configuration".to_string();
            self.generation_error = None;
            return;
        };

        let preset_id = preset.preset_id();
        let config = match self.preset_configs().get(preset_id) {
            Some(c) => c,
            None => {
                self.generation_error = Some(format!("Config not found for preset: {}", preset_id));
                return;
            }
        };

        let result = preset.generate(config, self.target_platform(), &self.language_version);

        match result {
            Ok(yaml) => {
                self.yaml_preview = yaml;
                self.generation_error = None;
            }
            Err(e) => {
                self.generation_error = Some(e.to_string());
            }
        }
    }

    fn has_any_options_enabled(&self, config: &PresetConfig) -> bool {
        config.values.values().any(|v| match v {
            OptionValue::Bool(b) => *b,
            _ => true, // Consider non-bool options as "enabled" if they have a value
        })
    }

    pub fn get_option_value(&self, preset_id: &str, option_id: &str) -> Option<&OptionValue> {
        self.preset_configs()
            .get(preset_id)
            .and_then(|config| config.get(option_id))
    }

    pub fn set_option_value(&mut self, preset_id: &str, option_id: &str, value: OptionValue) {
        if let Some(config) = self.preset_configs_mut().get_mut(preset_id) {
            config.set(option_id.to_string(), value);
            self.mark_active_persist();
        }
    }

    pub fn toggle_option(&mut self, preset_id: &str, option_id: &str) {
        if let Some(config) = self.preset_configs_mut().get_mut(preset_id) {
            if let Some(value) = config.get(option_id) {
                let new_value = match value {
                    OptionValue::Bool(b) => OptionValue::Bool(!b),
                    OptionValue::Enum { selected, variants } => {
                        // Cycle to next variant
                        let current_index =
                            variants.iter().position(|v| v == selected).unwrap_or(0);
                        let next_index = (current_index + 1) % variants.len();
                        OptionValue::Enum {
                            selected: variants[next_index].clone(),
                            variants: variants.clone(),
                        }
                    }
                    other => other.clone(),
                };
                config.set(option_id.to_string(), new_value);
                self.mark_active_persist();
            }
        }
        self.regenerate_yaml();
        self.auto_save_ron();
    }

    pub fn cycle_platform(&mut self) {
        let platforms = Platform::all();
        let current_index = platforms
            .iter()
            .position(|&p| p == self.target_platform())
            .unwrap_or(0);
        let next_index = (current_index + 1) % platforms.len();
        self.switch_to_platform(platforms[next_index]);
    }

    /// Make the pipeline for the given platform active, lazily creating a
    /// transient one (copied from the active pipeline) if none exists yet
    pub fn switch_to_platform(&mut self, platform: Platform) {
        if let Some(index) = self.pipelines.iter().position(|p| p.platform == platform) {
            self.active_pipeline = index;
        } else {
            let preset_configs = self.preset_configs().clone();
            self.pipelines.push(PipelineState {
                platform,
                preset_configs,
                persist: false,
            });
            self.active_pipeline = self.pipelines.len() - 1;
        }

        // Reload existing YAML for the new platform
        let output_path = self.working_dir.join(platform.output_path());
        self.existing_yaml = std::fs::read_to_string(&output_path).ok();

        self.regenerate_yaml();
        self.auto_save_ron();
    }

    pub fn toggle_preset(&mut self, preset_id: &str) {
        // Check if any options are currently enabled
        let config = match self.preset_configs().get(preset_id) {
            Some(c) => c,
            None => return,
        };

        let has_enabled = self.has_any_options_enabled(config);

        // Get preset to access all its fields
        let preset = match self.registry.get(preset_id) {
            Some(p) => p,
            None => return,
        };

        // Toggle all boolean options for this preset
        for field in preset.fields() {
            if matches!(field.default_value, OptionValue::Bool(_)) {
                self.set_option_value(preset_id, &field.id, OptionValue::Bool(!has_enabled));
            }
        }

        self.regenerate_yaml();
        self.auto_save_ron();
    }

    pub fn open_platform_menu(&mut self) {
        self.platform_menu_open = true;
    }

    pub fn close_platform_menu(&mut self) {
        self.platform_menu_open = false;
    }

    pub fn select_platform_from_menu(&mut self) {
        let platforms = Platform::all();
        if let Some(&platform) = platforms.get(self.platform_menu_cursor) {
            self.switch_to_platform(platform);
        }
        self.platform_menu_open = false;
    }

    pub fn update_current_item_description(&mut self) {
        self.current_item_description = match self.current_item() {
            Some(TreeItem::Category(_)) => String::new(),
            Some(TreeItem::Preset(preset_id)) => self
                .registry
                .get(preset_id)
                .map(|p| p.preset_description().to_string())
                .unwrap_or_default(),
            Some(TreeItem::Field(preset_id, field_id)) => self
                .registry
                .get(preset_id)
                .and_then(|preset| {
                    preset
                        .fields()
                        .into_iter()
                        .find(|f| &f.id == field_id)
                        .map(|f| f.description.clone())
                })
                .unwrap_or_default(),
            None => String::new(),
        };
    }

    pub fn scroll_preview_up(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_sub(1);
    }

    pub fn scroll_preview_down(&mut self) {
        self.preview_scroll = self.preview_scroll.saturating_add(1);
    }

    /// Check if an option value differs from its default value
    pub fn is_option_non_default(&self, preset_id: &str, option_id: &str) -> bool {
        let preset = match self.registry.get(preset_id) {
            Some(p) => p,
            None => return false,
        };

        let current_value = match self.get_option_value(preset_id, option_id) {
            Some(v) => v,
            None => return false,
        };

        // Find the option metadata to get default value
        for field in preset.fields() {
            if field.id == option_id {
                return current_value != &field.default_value;
            }
        }

        false
    }

    /// Check if a preset has any non-default options
    pub fn has_preset_non_defaults(&self, preset_id: &str) -> bool {
        let preset = match self.registry.get(preset_id) {
            Some(p) => p,
            None => return false,
        };

        for field in preset.fields() {
            if self.is_option_non_default(preset_id, &field.id) {
                return true;
            }
        }

        false
    }

    /// Auto-expand categories and presets that have non-default values
    pub fn auto_expand_non_defaults(&mut self) {
        let preset_ids: Vec<String> = self
            .registry
            .all()
            .into_iter()
            .map(|p| p.preset_id().to_string())
            .collect();

        for preset_id in preset_ids {
            if self.has_preset_non_defaults(&preset_id) {
                self.expanded_presets.insert(preset_id.clone());

                // Expand the category containing this preset
                if let Some(preset) = self.registry.get(&preset_id) {
                    self.expanded_categories
                        .insert(preset.preset_category().to_string());
                }
            }
        }

        self.rebuild_tree();
    }

    /// Load RON configuration into TUI state
    pub fn from_ron_file(path: &std::path::Path) -> Result<Self> {
        use crate::config::{parse_config, preset_choice_to_config};
        use anyhow::Context;

        let ron_str = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read RON file: {}", path.display()))?;

        let ron_config = match parse_config(&ron_str) {
            Ok(config) => config,
            Err(e) => {
                eprintln!("Warning: Failed to parse RON configuration: {}", e);
                eprintln!("This may be due to unknown or renamed fields in cibox.ron");
                eprintln!(
                    "Please check that all field names match the current preset struct definitions"
                );
                return Err(anyhow::anyhow!("Failed to parse RON configuration: {}", e));
            }
        };
        ron_config.validate()?;

        let registry = Arc::new(build_registry());

        // Every registered preset must have a config entry per pipeline; the
        // tree UI and toggles index into the map for all preset ids.
        let default_configs = |registry: &PresetRegistry| -> HashMap<String, PresetConfig> {
            registry
                .all()
                .into_iter()
                .map(|p| (p.preset_id().to_string(), p.default_config(false)))
                .collect()
        };

        let mut pipelines = Vec::new();
        for pipeline in &ron_config.0 {
            let mut preset_configs = default_configs(&registry);
            for preset_choice in &pipeline.presets {
                let (preset_id, config) = preset_choice_to_config(preset_choice);
                preset_configs.insert(preset_id, config);
            }
            pipelines.push(PipelineState {
                platform: pipeline.platform,
                preset_configs,
                persist: true,
            });
        }

        if pipelines.is_empty() {
            pipelines.push(PipelineState {
                platform: Platform::GitHub,
                preset_configs: default_configs(&registry),
                persist: false,
            });
        }

        let working_dir = path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let target_platform = pipelines[0].platform;

        // Try to load existing YAML file
        let output_path = working_dir.join(target_platform.output_path());
        let existing_yaml = std::fs::read_to_string(&output_path).ok();

        let mut state = Self {
            project_type: ProjectType::PythonApp, // Default, doesn't affect RON-loaded config
            language_version: "stable".to_string(),
            working_dir,
            pipelines,
            active_pipeline: 0,
            registry,
            expanded_categories: HashSet::new(),
            expanded_presets: HashSet::new(),
            tree_items: Vec::new(),
            tree_cursor: 0,
            platform_menu_open: false,
            platform_menu_cursor: Platform::all()
                .iter()
                .position(|&p| p == target_platform)
                .unwrap_or(0),
            preview_scroll: 0,
            yaml_preview: String::new(),
            generation_error: None,
            existing_yaml,
            current_item_description: String::new(),
            should_quit: false,
            should_write: false,
        };

        state.auto_expand_non_defaults();
        state.regenerate_yaml();
        state.update_current_item_description();
        Ok(state)
    }

    /// Enabled presets of a pipeline in registry order, for deterministic output
    pub fn enabled_preset_configs(&self, pipeline: &PipelineState) -> Vec<(String, PresetConfig)> {
        self.registry
            .all()
            .into_iter()
            .filter_map(|preset| {
                let preset_id = preset.preset_id();
                pipeline
                    .preset_configs
                    .get(preset_id)
                    .filter(|config| self.has_any_options_enabled(config))
                    .map(|config| (preset_id.to_string(), config.clone()))
            })
            .collect()
    }

    /// Export current TUI state to RON configuration
    pub fn export_to_ron(&self) -> Result<String> {
        use crate::config::{preset_config_to_choice, CiboxConfig, Pipeline};

        let mut pipelines = Vec::new();
        for platform in Platform::all() {
            let Some(pipeline) = self
                .pipelines
                .iter()
                .find(|p| p.platform == platform && p.persist)
            else {
                continue;
            };

            let presets: Vec<_> = self
                .enabled_preset_configs(pipeline)
                .iter()
                .map(|(preset_id, config)| preset_config_to_choice(preset_id, config))
                .collect();

            if !presets.is_empty() {
                pipelines.push(Pipeline { platform, presets });
            }
        }

        let ron_config = CiboxConfig(pipelines);

        let pretty_config = ron::ser::PrettyConfig::new()
            .depth_limit(5)
            .separate_tuple_members(true)
            .enumerate_arrays(false);

        let ron_str = crate::config::ron_options()
            .to_string_pretty(&ron_config, pretty_config)
            .map_err(|e| anyhow::anyhow!("Failed to serialize to RON: {}", e))?;

        Ok(ron_str)
    }

    /// Save current state to a RON file
    pub fn save_to_ron_file(&self, path: &std::path::Path) -> Result<()> {
        use anyhow::Context;

        let ron_str = self.export_to_ron()?;

        std::fs::write(path, ron_str)
            .with_context(|| format!("Failed to write RON file: {}", path.display()))?;

        Ok(())
    }

    /// Automatically save the current state to cibox.ron in the working directory
    pub fn auto_save_ron(&self) {
        let cibox_ron_path = self.working_dir.join("cibox.ron");

        // Silently attempt to save - don't panic on errors
        let _ = self.save_to_ron_file(&cibox_ron_path);
    }

    /// Find the category that contains a given preset
    pub fn preset_category(&self, preset_id: &str) -> Option<String> {
        self.registry
            .get(preset_id)
            .map(|p| p.preset_category().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tempfile::tempdir;

    #[test]
    fn test_rust_library_enables_only_matching_presets() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        // All presets should be shown in the tree (under categories)
        let preset_items: Vec<&str> = state
            .tree_items
            .iter()
            .filter_map(|item| match item {
                TreeItem::Preset(preset_id) => Some(preset_id.as_str()),
                _ => None,
            })
            .collect();

        // Only presets in expanded categories show up
        // Rust matches, so Languages category is expanded
        assert!(preset_items.contains(&"Rust"));

        // Rust options should be enabled by default when detected
        let rust_config = state.preset_configs().get("Rust").unwrap();
        // Defaults are false per preset_field, but set via default_config(detected=true)
        assert!(rust_config.values.contains_key("enable_coverage"));

        let python_config = state.preset_configs().get("PythonApp").unwrap();
        assert!(!python_config.get_bool("enable_type_check"));
    }

    #[test]
    fn test_rust_binary_enables_only_matching_presets() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustBinary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        let rust_config = state.preset_configs().get("Rust").unwrap();
        assert!(rust_config.values.contains_key("enable_linter"));

        let python_config = state.preset_configs().get("PythonApp").unwrap();
        assert!(!python_config.get_bool("enable_type_check"));
    }

    #[test]
    fn test_python_app_enables_only_matching_presets() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::PythonApp,
            language_version: Some("3.11".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        let rust_config = state.preset_configs().get("Rust").unwrap();
        assert!(!rust_config.get_bool("enable_coverage"));

        // Python detected, so its bool defaults apply
        let python_config = state.preset_configs().get("PythonApp").unwrap();
        assert!(python_config.values.contains_key("enable_type_check"));
    }

    #[test]
    fn test_go_app_enables_only_matching_presets() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::GoApp,
            language_version: Some("1.21".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        let go_config = state.preset_configs().get("GoApp").unwrap();
        assert!(go_config.get_bool("enable_linter"));
        assert!(go_config.get_bool("enable_security_scan"));
    }

    #[test]
    fn test_docker_preset_shown_for_all_project_types() {
        let project_types = vec![
            ProjectType::RustLibrary,
            ProjectType::RustBinary,
            ProjectType::PythonApp,
            ProjectType::GoApp,
        ];

        for project_type in project_types {
            let dir = tempdir().unwrap();

            let detection = DetectionResult {
                project_type: project_type.clone(),
                language_version: Some("stable".to_string()),
                metadata: HashMap::new(),
            };

            let state =
                EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

            // Docker config should exist even if not shown in tree (category not expanded)
            let docker_config = state.preset_configs().get("Docker");
            assert!(
                docker_config.is_some(),
                "Docker preset should be available for {:?}",
                project_type
            );
        }
    }

    #[test]
    fn test_docker_enabled_with_docker_project_type() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::DockerImage,
            language_version: None,
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        let docker_config = state.preset_configs().get("Docker").unwrap();
        assert!(docker_config.get_bool("enable_cache"));
    }

    #[test]
    fn test_docker_disabled_for_non_docker_project() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        let docker_config = state.preset_configs().get("Docker").unwrap();
        // Docker preset is available but not enabled by default for non-Docker projects
        assert!(!docker_config.get_bool("enable_cache"));
    }

    #[test]
    fn test_docker_preset_available_for_all_projects() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        // Docker config should exist
        let docker_config = state.preset_configs().get("Docker");
        assert!(docker_config.is_some(), "Docker preset should be available");

        // But not enabled by default
        assert!(!docker_config.unwrap().get_bool("enable_cache"));
    }

    #[test]
    fn test_categories_in_tree() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let state = EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        // Should have category items in the tree
        let category_items: Vec<&str> = state
            .tree_items
            .iter()
            .filter_map(|item| match item {
                TreeItem::Category(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();

        // At least Languages and Packaging categories should exist
        assert!(category_items.contains(&"Languages"));
        assert!(category_items.contains(&"Packaging"));
    }

    #[test]
    fn test_manually_enabling_non_detected_preset_generates_yaml() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let mut state =
            EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        // Enable Rust options first
        use crate::editor::config::OptionValue;
        state.set_option_value("Rust", "enable_linter", OptionValue::Bool(true));
        state.regenerate_yaml();

        // The YAML should be for Rust
        assert!(state.yaml_preview.contains("cargo"));

        // Now disable Rust and enable Python App
        state.set_option_value("Rust", "enable_linter", OptionValue::Bool(false));
        state.set_option_value("PythonApp", "enable_type_check", OptionValue::Bool(true));

        state.regenerate_yaml();

        // The YAML should now be for Python, even though it's not the detected type
        assert!(
            state.yaml_preview.contains("python")
                || state.yaml_preview.contains("pytest")
                || state.yaml_preview.contains("Setup Python")
        );
        assert!(!state.yaml_preview.contains("cargo"));
    }

    #[test]
    fn test_multiple_presets_first_enabled_wins() {
        let dir = tempdir().unwrap();

        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };

        let mut state =
            EditorState::from_detection(detection, None, dir.path().to_path_buf()).unwrap();

        // Enable both Rust and Python (unusual but allowed)
        use crate::editor::config::OptionValue;
        state.set_option_value("Rust", "enable_linter", OptionValue::Bool(true));
        state.set_option_value("PythonApp", "enable_type_check", OptionValue::Bool(true));

        state.regenerate_yaml();

        // The first preset with options enabled should be used (registry order)
        assert!(state.yaml_preview.contains("cargo"));
    }

    fn rust_state(dir: &std::path::Path) -> EditorState {
        let detection = DetectionResult {
            project_type: ProjectType::RustLibrary,
            language_version: Some("stable".to_string()),
            metadata: HashMap::new(),
        };
        EditorState::from_detection(detection, None, dir.to_path_buf()).unwrap()
    }

    #[test]
    fn test_pipeline_settings_are_independent() {
        use crate::editor::config::OptionValue;
        let dir = tempdir().unwrap();
        let mut state = rust_state(dir.path());

        state.set_option_value("Rust", "enable_coverage", OptionValue::Bool(true));

        state.switch_to_platform(Platform::GitLab);
        state.set_option_value("Rust", "enable_coverage", OptionValue::Bool(false));
        state.set_option_value("Rust", "enable_linter", OptionValue::Bool(true));

        assert!(!state.preset_configs().get("Rust").unwrap().get_bool("enable_coverage"));

        state.switch_to_platform(Platform::GitHub);
        assert!(state.preset_configs().get("Rust").unwrap().get_bool("enable_coverage"));
        assert!(!state.preset_configs().get("Rust").unwrap().get_bool("enable_linter"));
    }

    #[test]
    fn test_browsing_platforms_does_not_persist() {
        let dir = tempdir().unwrap();
        let mut state = rust_state(dir.path());

        state.switch_to_platform(Platform::Jenkins);
        assert!(!state.has_pipeline(Platform::Jenkins));
        assert_eq!(state.persisted_pipeline_count(), 1);

        // Modifying the transient pipeline persists it
        state.toggle_option("Rust", "enable_linter");
        assert!(state.has_pipeline(Platform::Jenkins));
        assert_eq!(state.persisted_pipeline_count(), 2);
    }

    #[test]
    fn test_export_to_ron_round_trips_multiple_pipelines() {
        use crate::editor::config::OptionValue;
        let dir = tempdir().unwrap();
        let mut state = rust_state(dir.path());

        state.set_option_value("Rust", "enable_coverage", OptionValue::Bool(true));
        state.switch_to_platform(Platform::GitLab);
        state.set_option_value("Rust", "enable_coverage", OptionValue::Bool(false));
        state.set_option_value("Rust", "enable_linter", OptionValue::Bool(true));

        let ron_str = state.export_to_ron().unwrap();
        let config = crate::config::parse_config(&ron_str).unwrap();
        config.validate().unwrap();
        assert_eq!(config.pipeline_count(), 2);

        let coverage_for = |platform| {
            let pipeline = config.pipeline_for(platform).unwrap();
            let (_, preset_config) = pipeline.presets[0].to_preset_config();
            preset_config.get_bool("enable_coverage")
        };
        assert!(coverage_for(Platform::GitHub));
        assert!(!coverage_for(Platform::GitLab));
    }
}
