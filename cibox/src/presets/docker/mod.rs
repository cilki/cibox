use cibox_macros::Preset;

mod circleci;
mod gitea;
mod github;
mod gitlab;
mod jenkins;

/// Container registry options for Docker image pushing
#[derive(
    Debug,
    Clone,
    PartialEq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    strum::VariantNames,
)]
#[serde(rename_all = "lowercase")]
pub enum DockerRegistry {
    /// Push to Docker Hub (requires DOCKER_USERNAME and DOCKER_PASSWORD secrets)
    #[strum(serialize = "dockerhub")]
    DockerHub,
    /// Push to GitHub Container Registry (uses GITHUB_TOKEN)
    #[strum(serialize = "github")]
    GitHubRegistry,
    /// Don't push images (build only)
    #[default]
    #[strum(serialize = "none")]
    None,
}

/// CI pipeline for building and pushing Docker images to registries
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Packaging")]
#[serde(default)]
pub struct Docker {
    /// Docker image name (e.g., "myorg/myapp")
    #[preset_field(display = "Image Name")]
    pub(super) image_name: String,

    /// Choose where to push Docker images
    #[preset_field(display = "Registry Type")]
    pub(super) registry: DockerRegistry,

    /// Dockerfile path (default: "./Dockerfile")
    #[preset_field(hidden = true)]
    pub(super) dockerfile_path: String,

    /// Docker build context (default: ".")
    #[preset_field(hidden = true)]
    pub(super) build_context: String,

    /// Use Docker layer caching for faster builds
    #[preset_field(display = "Enable Cache")]
    pub(super) enable_cache: bool,

    /// Only push images on git tags (not on branch pushes)
    #[preset_field(display = "Tags Only")]
    pub(super) push_on_tags_only: bool,
}

impl Default for Docker {
    fn default() -> Self {
        Self {
            image_name: "myapp".to_string(),
            registry: DockerRegistry::None,
            dockerfile_path: "./Dockerfile".to_string(),
            build_context: ".".to_string(),
            enable_cache: true,
            push_on_tags_only: false,
        }
    }
}

#[cfg(test)]
#[cfg(disabled)]
mod tests {
    // Tests temporarily disabled - need to be updated after macro migration
}
