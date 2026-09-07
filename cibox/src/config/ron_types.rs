use serde::{Deserialize, Serialize};

use crate::config::Platform;
use crate::error::Result;
use crate::presets::{Docker, Gitleaks, GoApp, PythonApp, Rust};

/// Top-level cibox configuration: an array of pipelines, one per CI platform
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CiboxConfig(pub Vec<Pipeline>);

/// A single CI pipeline targeting one platform
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pipeline {
    /// Target CI platform
    pub platform: Platform,
    /// List of preset configurations for this pipeline
    pub presets: Vec<PresetChoice>,
}

impl CiboxConfig {
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|p| p.presets.is_empty())
    }

    pub fn len(&self) -> usize {
        self.0.iter().map(|p| p.presets.len()).sum()
    }

    pub fn pipeline_count(&self) -> usize {
        self.0.len()
    }

    pub fn pipeline_for(&self, platform: Platform) -> Option<&Pipeline> {
        self.0.iter().find(|p| p.platform == platform)
    }

    /// Reject configurations with more than one pipeline per platform
    pub fn validate(&self) -> Result<()> {
        let mut seen = std::collections::HashSet::new();
        for pipeline in &self.0 {
            if !seen.insert(pipeline.platform) {
                return Err(crate::error::validation_error(format!(
                    "Duplicate pipeline for platform '{}'",
                    pipeline.platform
                )));
            }
        }
        Ok(())
    }
}

/// Parse a cibox.ron document
pub fn parse_config(ron_str: &str) -> Result<CiboxConfig> {
    Ok(crate::config::ron_options().from_str(ron_str)?)
}

/// Preset choice enum - supports all available presets
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PresetChoice {
    #[serde(rename = "Python")]
    PythonApp(PythonApp),
    Rust(Rust),
    GoApp(GoApp),
    Docker(Docker),
    Gitleaks(Gitleaks),
}

macro_rules! preset_choice_tables {
    ($(($variant:ident, $ty:path, $display:literal)),+ $(,)?) => {
        impl PresetChoice {
            /// Convert a PresetChoice to a PresetConfig using the generated conversion methods
            pub fn to_preset_config(&self) -> (String, crate::editor::config::PresetConfig) {
                match self {
                    $(PresetChoice::$variant(preset) => {
                        (stringify!($variant).to_string(), preset.to_preset_config())
                    })+
                }
            }

            /// Human-readable name for display in CLI output
            pub fn display_name(&self) -> &'static str {
                match self {
                    $(PresetChoice::$variant(_) => $display,)+
                }
            }
        }

        /// Convert a (preset_id, PresetConfig) tuple to a PresetChoice
        pub fn preset_config_to_choice(
            preset_id: &str,
            config: &crate::editor::config::PresetConfig,
        ) -> PresetChoice {
            match preset_id {
                $(stringify!($variant) => PresetChoice::$variant(<$ty>::from_config(config, "")),)+
                _ => panic!("Unknown preset ID: {}", preset_id),
            }
        }
    };
}
crate::presets::with_presets!(preset_choice_tables);

/// Convert a PresetChoice to a (preset_id, PresetConfig) tuple
pub fn preset_choice_to_config(
    choice: &PresetChoice,
) -> (String, crate::editor::config::PresetConfig) {
    choice.to_preset_config()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_single_paren_presets() {
        let config = parse_config(
            r#"[
                (
                    platform: GitHub,
                    presets: [
                        Rust(enable_linter: true),
                        Python(linter: ruff),
                        Docker(registry: githubregistry),
                    ],
                ),
            ]"#,
        )
        .unwrap();
        assert_eq!(config.pipeline_count(), 1);
        assert_eq!(config.len(), 3);
        assert_eq!(config.0[0].platform, Platform::GitHub);
    }

    #[test]
    fn test_serialize_uses_single_parens() {
        let config = CiboxConfig(vec![Pipeline {
            platform: Platform::GitHub,
            presets: vec![PresetChoice::Rust(Rust::default())],
        }]);
        let ron_str = crate::config::ron_options()
            .to_string_pretty(&config, ron::ser::PrettyConfig::new())
            .unwrap();
        assert!(ron_str.trim_start().starts_with('['), "{ron_str}");
        assert!(ron_str.contains("platform: GitHub"), "{ron_str}");
        assert!(ron_str.contains("Rust("), "{ron_str}");
        assert!(!ron_str.contains("Rust(("), "{ron_str}");

        let parsed = parse_config(&ron_str).unwrap();
        assert_eq!(parsed.len(), 1);
    }

    #[test]
    fn test_parse_multiple_pipelines_with_different_settings() {
        let config = parse_config(
            r#"[
                ( platform: GitHub, presets: [ Rust(enable_coverage: true) ] ),
                ( platform: GitLab, presets: [ Rust(enable_coverage: false) ] ),
            ]"#,
        )
        .unwrap();
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

    #[test]
    fn test_legacy_format_rejected() {
        let result = parse_config(
            r#"(
                version: "1",
                presets: [ Rust(enable_linter: true) ],
            )"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_duplicate_platform_rejected() {
        let config = parse_config(
            r#"[
                ( platform: GitHub, presets: [] ),
                ( platform: GitHub, presets: [] ),
            ]"#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }
}
