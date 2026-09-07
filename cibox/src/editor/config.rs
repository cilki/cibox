use crate::detection::ProjectType;
use crate::editor::state::Platform;
use crate::error::Result;
use std::collections::HashMap;

/// Represents a configurable option value
#[derive(Debug, Clone, PartialEq)]
pub enum OptionValue {
    Bool(bool),
    Enum {
        selected: String,
        variants: Vec<String>,
    },
    String(String),
    Int(i32),
}

impl OptionValue {
    pub fn as_bool(&self) -> bool {
        matches!(self, OptionValue::Bool(true))
    }

    pub fn toggle_bool(&mut self) {
        if let OptionValue::Bool(v) = self {
            *v = !*v;
        }
    }

    pub fn cycle_enum(&mut self) {
        if let OptionValue::Enum { selected, variants } = self {
            let idx = variants.iter().position(|v| v == selected).unwrap_or(0);
            let next_idx = (idx + 1) % variants.len();
            *selected = variants[next_idx].clone();
        }
    }
}

/// Metadata for a single option within a preset
#[derive(Debug, Clone)]
pub struct OptionMeta {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub default_value: OptionValue,
    pub depends_on: Option<String>, // ID of parent option this depends on
}

/// Runtime configuration state for a preset
#[derive(Debug, Clone)]
pub struct PresetConfig {
    /// Preset ID this config belongs to
    pub preset_id: String,
    /// Flat map of option_id -> value
    pub values: HashMap<String, OptionValue>,
    /// Track which options have been explicitly set (vs using defaults)
    pub explicitly_set: std::collections::HashSet<String>,
}

impl PresetConfig {
    pub fn new(preset_id: String) -> Self {
        Self {
            preset_id,
            values: HashMap::new(),
            explicitly_set: std::collections::HashSet::new(),
        }
    }

    pub fn get(&self, option_id: &str) -> Option<&OptionValue> {
        self.values.get(option_id)
    }

    pub fn get_bool(&self, option_id: &str) -> bool {
        self.values
            .get(option_id)
            .map(|v| v.as_bool())
            .unwrap_or(false)
    }

    pub fn get_string(&self, option_id: &str) -> Option<String> {
        self.values.get(option_id).and_then(|v| {
            if let OptionValue::String(s) = v {
                Some(s.clone())
            } else {
                None
            }
        })
    }

    pub fn get_enum(&self, option_id: &str) -> Option<String> {
        self.values.get(option_id).and_then(|v| {
            if let OptionValue::Enum { selected, .. } = v {
                Some(selected.clone())
            } else {
                None
            }
        })
    }

    pub fn set(&mut self, option_id: String, value: OptionValue) {
        self.explicitly_set.insert(option_id.clone());
        self.values.insert(option_id, value);
    }

    pub fn toggle(&mut self, option_id: &str) {
        if let Some(value) = self.values.get_mut(option_id) {
            self.explicitly_set.insert(option_id.to_string());
            match value {
                OptionValue::Bool(b) => *b = !*b,
                OptionValue::Enum { .. } => value.cycle_enum(),
                _ => {}
            }
        }
    }

    pub fn is_explicitly_set(&self, option_id: &str) -> bool {
        self.explicitly_set.contains(option_id)
    }
}

/// Trait that all presets must implement for editor integration
pub trait EditorPreset: Send + Sync {
    /// Unique identifier for this preset
    fn preset_id(&self) -> &'static str;

    /// Human-readable name
    fn preset_name(&self) -> &'static str;

    /// Description shown in UI
    fn preset_description(&self) -> &'static str;

    /// Category for grouping in the TUI (e.g., "Languages", "Packaging")
    fn preset_category(&self) -> &'static str;

    /// Define the fields for this preset
    fn fields(&self) -> Vec<OptionMeta>;

    /// Build the preset with given configuration and produce its
    /// platform-neutral jobs (combinable with other presets' jobs)
    fn build_jobs(
        &self,
        config: &PresetConfig,
        language_version: &str,
    ) -> Result<crate::ir::PresetJobs>;

    /// Build the preset with given configuration and generate rendered output
    fn generate(
        &self,
        config: &PresetConfig,
        platform: Platform,
        language_version: &str,
    ) -> Result<String> {
        let jobs = self.build_jobs(config, language_version)?;
        crate::platforms::lower::lower(platform, &[jobs])?.render()
    }

    /// Check if this preset matches the detected project type
    /// This is used for UI coloring and sorting, not for enabling/disabling presets
    fn matches_project(&self, project_type: &ProjectType, working_dir: &std::path::Path) -> bool;

    /// Get default configuration based on project detection
    fn default_config(&self, detected: bool) -> PresetConfig;
}
