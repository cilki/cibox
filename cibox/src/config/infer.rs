use crate::config::Platform;
use crate::detection::{ProjectFacts, RemoteHost};

/// Pick a target platform when cibox.ron doesn't name one: existing CI
/// configuration wins, then the git remote host, then GitHub.
pub fn infer_platform(facts: &ProjectFacts) -> Platform {
    if let Some(&platform) = facts.existing_ci.first() {
        return platform;
    }
    match facts.remote_host {
        Some(RemoteHost::GitHub) => Platform::GitHub,
        Some(RemoteHost::GitLab) => Platform::GitLab,
        Some(RemoteHost::Gitea) => Platform::Gitea,
        _ => Platform::GitHub,
    }
}

/// Why `cibox update` targets the platforms it does
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    /// Named with `--github`, `--gitlab`, ...
    Flags,
    /// Every platform with a cibox-managed file already on disk
    Existing,
    /// Nothing on disk yet: one platform, from [`infer_platform`]
    Inferred,
}

impl TargetSource {
    pub fn describe(self) -> &'static str {
        match self {
            TargetSource::Flags => "from flags",
            TargetSource::Existing => "existing cibox pipelines",
            TargetSource::Inferred => "inferred",
        }
    }
}

/// The platforms `cibox update` writes: the ones `requested` by flag; with
/// no flags, every platform that already has a cibox-managed file; and when
/// there are none either, the single inferred platform, so a fresh project
/// still gets a pipeline.
pub fn target_platforms(
    facts: &ProjectFacts,
    requested: &[Platform],
) -> (Vec<Platform>, TargetSource) {
    if !requested.is_empty() {
        // Flag order is meaningless; keep the canonical order, deduplicated
        let platforms = Platform::all()
            .into_iter()
            .filter(|p| requested.contains(p))
            .collect();
        return (platforms, TargetSource::Flags);
    }
    if !facts.managed_ci.is_empty() {
        return (facts.managed_ci.clone(), TargetSource::Existing);
    }
    (vec![infer_platform(facts)], TargetSource::Inferred)
}

/// Platform names joined for display, e.g. "GitHub Actions, GitLab CI"
pub fn platform_names(platforms: &[Platform]) -> String {
    platforms
        .iter()
        .map(|p| p.name())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flags_win_over_existing_pipelines() {
        let facts = ProjectFacts {
            managed_ci: vec![Platform::GitHub],
            ..ProjectFacts::default()
        };
        assert_eq!(
            target_platforms(
                &facts,
                &[Platform::CircleCI, Platform::GitLab, Platform::GitLab]
            ),
            (
                vec![Platform::GitLab, Platform::CircleCI],
                TargetSource::Flags
            )
        );
    }

    #[test]
    fn test_no_flags_targets_every_existing_pipeline() {
        let facts = ProjectFacts {
            managed_ci: vec![Platform::GitHub, Platform::GitLab],
            existing_ci: vec![Platform::CircleCI],
            ..ProjectFacts::default()
        };
        assert_eq!(
            target_platforms(&facts, &[]),
            (
                vec![Platform::GitHub, Platform::GitLab],
                TargetSource::Existing
            )
        );
    }

    #[test]
    fn test_no_flags_and_no_pipeline_infers_one_platform() {
        let facts = ProjectFacts {
            remote_host: Some(RemoteHost::Gitea),
            ..ProjectFacts::default()
        };
        assert_eq!(
            target_platforms(&facts, &[]),
            (vec![Platform::Gitea], TargetSource::Inferred)
        );
    }

    #[test]
    fn test_default_is_github() {
        assert_eq!(infer_platform(&ProjectFacts::default()), Platform::GitHub);
    }

    #[test]
    fn test_remote_host_wins_without_existing_ci() {
        let facts = ProjectFacts {
            remote_host: Some(RemoteHost::GitLab),
            ..ProjectFacts::default()
        };
        assert_eq!(infer_platform(&facts), Platform::GitLab);
    }

    #[test]
    fn test_existing_ci_wins_over_remote() {
        let facts = ProjectFacts {
            existing_ci: vec![Platform::CircleCI],
            remote_host: Some(RemoteHost::GitLab),
            ..ProjectFacts::default()
        };
        assert_eq!(infer_platform(&facts), Platform::CircleCI);
    }
}
