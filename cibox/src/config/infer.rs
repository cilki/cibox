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

#[cfg(test)]
mod tests {
    use super::*;

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
            existing_ci: vec![Platform::Jenkins],
            remote_host: Some(RemoteHost::GitLab),
            ..ProjectFacts::default()
        };
        assert_eq!(infer_platform(&facts), Platform::Jenkins);
    }
}
