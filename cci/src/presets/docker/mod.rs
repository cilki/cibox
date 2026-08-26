use crate::traits::PresetInfo;
use cci_macros::{Preset, PresetEnum};

mod circleci;
mod detectable;
mod gitea;
mod github;
mod gitlab;
mod jenkins;

/// Container registry options for Docker image pushing
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, PresetEnum)]
#[preset_enum(default = "None")]
#[serde(rename_all = "lowercase")]
pub enum DockerRegistry {
    #[preset_variant(id = "dockerhub", display = "Docker Hub")]
    /// Push to Docker Hub (requires DOCKER_USERNAME and DOCKER_PASSWORD secrets)
    DockerHub,
    #[preset_variant(id = "github", display = "GitHub Container Registry")]
    /// Push to GitHub Container Registry (uses GITHUB_TOKEN)
    GitHubRegistry,
    #[preset_variant(id = "none", display = "None (build only)")]
    /// Don't push images (build only)
    None,
}

/// CI pipeline for building and pushing Docker images to registries
#[derive(Debug, Clone, Preset)]
#[preset(category = "Packaging")]
pub struct Docker {
    /// Docker image name (e.g., "myorg/myapp")
    #[preset_field(display = "Image Name", default = "\"myapp\".to_string()")]
    pub(super) image_name: String,

    /// Choose where to push Docker images
    #[preset_field(display = "Registry Type", default = "DockerRegistry::None")]
    pub(super) registry: DockerRegistry,

    #[preset_field(hidden = true, default = "\"./Dockerfile\".to_string()")]
    /// Dockerfile path (default: "./Dockerfile")
    pub(super) dockerfile_path: String,

    #[preset_field(hidden = true, default = "\".\".to_string()")]
    /// Docker build context (default: ".")
    pub(super) build_context: String,

    /// Use Docker layer caching for faster builds
    #[preset_field(display = "Enable Cache", default = "true")]
    pub(super) enable_cache: bool,

    /// Only push images on git tags (not on branch pushes)
    #[preset_field(display = "Tags Only", default = "false")]
    pub(super) push_on_tags_only: bool,
}

impl Docker {
    /// Constant default instance for registry initialization
    pub const DEFAULT: Self = Self {
        image_name: String::new(),
        registry: DockerRegistry::None,
        dockerfile_path: String::new(),
        build_context: String::new(),
        enable_cache: false,
        push_on_tags_only: false,
    };
}

impl PresetInfo for Docker {
    fn name(&self) -> &str {
        "Docker"
    }

    fn description(&self) -> &str {
        "CI pipeline for building and pushing Docker images to registries"
    }
}
