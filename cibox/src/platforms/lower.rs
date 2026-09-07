//! Lowering from the platform-neutral IR (`crate::ir`) to platform models.
//!
//! Cross-platform policy lives here: job keys are namespaced as
//! `{preset slug}-{job id}`, and trigger rules are defined once.

use crate::config::Platform;
use crate::error::Result;
use crate::ir::PresetJobs;
use crate::platforms::helpers::PlatformConfig;

/// Lower one or more presets' jobs into a single platform config.
///
/// GitLab/CircleCI/Jenkins are single-file platforms: pass all presets at
/// once. GitHub/Gitea emit one file per preset: call once per preset.
pub fn lower(platform: Platform, presets: &[PresetJobs]) -> Result<PlatformConfig> {
    Ok(match platform {
        Platform::GitHub => PlatformConfig::GitHub(super::github::lower::lower_github(presets)),
        // Gitea Actions uses the same workflow format as GitHub Actions
        Platform::Gitea => PlatformConfig::Gitea(super::github::lower::lower_github(presets)),
        Platform::GitLab => PlatformConfig::GitLab(super::gitlab::lower::lower_gitlab(presets)),
        Platform::CircleCI => {
            PlatformConfig::CircleCI(super::circleci::lower::lower_circleci(presets))
        }
        Platform::Jenkins => {
            PlatformConfig::Jenkins(super::jenkins::lower::lower_jenkins(presets))
        }
    })
}

/// Namespaced job key: "rust" + "test" → "rust-test"
pub(crate) fn job_key(slug: &str, job_id: &str) -> String {
    format!("{slug}-{job_id}")
}
