use crate::config::Platform;
use crate::editor::config::PresetConfig;
use crate::editor::registry::PresetRegistry;
use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

/// Generates CI configurations for multiple presets
pub struct MultiPresetGenerator {
    preset_configs: Vec<(String, PresetConfig)>,
    registry: Arc<PresetRegistry>,
    platform: Platform,
    language_version: String,
}

impl MultiPresetGenerator {
    pub fn new(
        preset_configs: Vec<(String, PresetConfig)>,
        registry: Arc<PresetRegistry>,
        platform: Platform,
        language_version: String,
    ) -> Self {
        Self {
            preset_configs,
            registry,
            platform,
            language_version,
        }
    }

    /// Generate all preset configurations
    /// Returns a vector of (filename, content) tuples
    pub fn generate_all(&self) -> Result<Vec<(PathBuf, String)>> {
        match self.platform {
            // GitHub and Gitea support multiple workflow files, one per preset
            Platform::GitHub | Platform::Gitea => {
                let mut outputs = Vec::new();
                for (preset_id, config) in &self.preset_configs {
                    if let Some(preset) = self.registry.get(preset_id) {
                        let yaml = preset.generate(config, self.platform, &self.language_version)?;
                        outputs.push((Self::derive_filename(preset_id, self.platform), yaml));
                    }
                }
                Ok(outputs)
            }
            // GitLab, CircleCI, and Jenkins each read a single file, so all
            // presets' jobs are lowered into one document
            Platform::GitLab | Platform::CircleCI | Platform::Jenkins => {
                let mut all_jobs = Vec::new();
                for (preset_id, config) in &self.preset_configs {
                    if let Some(preset) = self.registry.get(preset_id) {
                        all_jobs.push(preset.build_jobs(config, &self.language_version)?);
                    }
                }
                if all_jobs.is_empty() {
                    return Ok(Vec::new());
                }
                let lowered = crate::platforms::lower::lower(self.platform, &all_jobs)?;
                Ok(vec![(self.platform.output_path(), lowered.render()?)])
            }
        }
    }

    /// Derive the output filename based on preset ID and platform
    pub fn derive_filename(preset_id: &str, platform: Platform) -> PathBuf {
        match platform {
            Platform::GitHub => PathBuf::from(format!(".github/workflows/{}.yml", preset_id)),
            Platform::Gitea => PathBuf::from(format!(".gitea/workflows/{}.yml", preset_id)),
            Platform::GitLab | Platform::CircleCI | Platform::Jenkins => platform.output_path(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::registry::build_registry;
    use std::sync::Arc;

    fn rust_and_docker_configs(registry: &crate::editor::registry::PresetRegistry) -> Vec<(String, PresetConfig)> {
        ["Rust", "Docker"]
            .iter()
            .map(|id| {
                let mut config = registry.get(id).unwrap().default_config(true);
                config.set(
                    "enable_linter".to_string(),
                    crate::editor::config::OptionValue::Bool(true),
                );
                (id.to_string(), config)
            })
            .collect()
    }

    #[test]
    fn test_multiple_presets_merge_into_single_gitlab_file() {
        let registry = Arc::new(build_registry());
        let preset_configs = rust_and_docker_configs(&registry);

        let generator = MultiPresetGenerator::new(
            preset_configs,
            registry,
            Platform::GitLab,
            "stable".to_string(),
        );
        let outputs = generator.generate_all().unwrap();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, PathBuf::from(".gitlab-ci.yml"));
        let yaml = &outputs[0].1;
        assert!(yaml.contains("rust-test"), "{yaml}");
        assert!(yaml.contains("docker-build"), "{yaml}");
        // A valid single YAML document with one merged stages list
        assert_eq!(yaml.matches("stages:").count(), 1, "{yaml}");
        serde_yaml::from_str::<crate::platforms::gitlab::models::GitLabCI>(yaml).unwrap();
    }

    #[test]
    fn test_multiple_presets_merge_into_single_jenkinsfile() {
        let registry = Arc::new(build_registry());
        let preset_configs = rust_and_docker_configs(&registry);

        let generator = MultiPresetGenerator::new(
            preset_configs,
            registry,
            Platform::Jenkins,
            "stable".to_string(),
        );
        let outputs = generator.generate_all().unwrap();

        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].0, PathBuf::from("Jenkinsfile"));
        let groovy = &outputs[0].1;
        assert_eq!(groovy.matches("pipeline {").count(), 1, "{groovy}");
        assert!(groovy.contains("stage('Rust: "), "{groovy}");
        assert!(groovy.contains("stage('Docker: "), "{groovy}");
    }

    #[test]
    fn test_repo_config_generates_for_every_platform() {
        // The repo's own cibox.ron should lower and render on all platforms
        let config = crate::config::ron_types::parse_config(include_str!("../../../cibox.ron"))
            .expect("repo cibox.ron should parse");
        let registry = Arc::new(build_registry());

        let preset_configs: Vec<(String, PresetConfig)> = config
            .0
            .iter()
            .flat_map(|p| p.presets.iter())
            .map(|choice| choice.to_preset_config())
            .collect();
        assert!(!preset_configs.is_empty());

        for platform in [
            Platform::GitHub,
            Platform::Gitea,
            Platform::GitLab,
            Platform::CircleCI,
            Platform::Jenkins,
        ] {
            let generator = MultiPresetGenerator::new(
                preset_configs.clone(),
                registry.clone(),
                platform,
                "stable".to_string(),
            );
            let outputs = generator.generate_all().unwrap();
            assert!(!outputs.is_empty(), "{platform:?} produced no output");
            for (path, content) in outputs {
                match platform {
                    Platform::Jenkins => {
                        assert_eq!(content.matches("pipeline {").count(), 1, "{path:?}")
                    }
                    _ => {
                        serde_yaml::from_str::<serde_yaml::Value>(&content)
                            .unwrap_or_else(|e| panic!("{path:?} is not valid YAML: {e}"));
                    }
                }
            }
        }
    }

    #[test]
    fn test_github_still_emits_one_file_per_preset() {
        let registry = Arc::new(build_registry());
        let preset_configs = rust_and_docker_configs(&registry);

        let generator = MultiPresetGenerator::new(
            preset_configs,
            registry,
            Platform::GitHub,
            "stable".to_string(),
        );
        let outputs = generator.generate_all().unwrap();

        let files: Vec<_> = outputs.iter().map(|(f, _)| f.clone()).collect();
        assert_eq!(
            files,
            vec![
                PathBuf::from(".github/workflows/Rust.yml"),
                PathBuf::from(".github/workflows/Docker.yml"),
            ]
        );
    }
}
