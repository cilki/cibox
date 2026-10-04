//! Lowering from the platform-neutral IR (`crate::ir`) to platform models.
//!
//! Jobs arrive with their final keys (the rule ids); trigger rules and
//! platform idioms are defined here and in the per-platform modules.

use crate::config::Platform;
use crate::error::Result;
use crate::ir::Job;
use crate::platforms::github::lower::WorkflowKind;
use crate::platforms::helpers::PlatformConfig;

/// Lower jobs into a single platform config document.
///
/// GitLab/CircleCI gate tags-only jobs within the one document. For
/// GitHub/Gitea this produces a CI workflow where tags-only jobs are guarded
/// by an `if:` expression; the generator normally splits them into a separate
/// release workflow instead.
pub fn lower(platform: Platform, jobs: &[Job]) -> Result<PlatformConfig> {
    Ok(match platform {
        Platform::GitHub => {
            PlatformConfig::GitHub(super::github::lower::lower_github(jobs, WorkflowKind::Ci))
        }
        // Gitea Actions uses the same workflow format as GitHub Actions
        Platform::Gitea => {
            PlatformConfig::Gitea(super::github::lower::lower_github(jobs, WorkflowKind::Ci))
        }
        Platform::GitLab => PlatformConfig::GitLab(super::gitlab::lower::lower_gitlab(jobs)),
        Platform::CircleCI => {
            PlatformConfig::CircleCI(super::circleci::lower::lower_circleci(jobs))
        }
    })
}
