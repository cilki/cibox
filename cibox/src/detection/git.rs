use std::fs;
use std::path::Path;

/// Forge hosting the origin remote
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteHost {
    GitHub,
    GitLab,
    Gitea,
    Other,
}

#[derive(Debug, Clone, Default)]
pub(super) struct GitFacts {
    pub is_repo: bool,
    pub repo_slug: Option<String>,
    pub remote_host: Option<RemoteHost>,
}

/// Read git facts straight from `.git/config` — no git subprocess needed
pub(super) fn gather(path: &Path) -> GitFacts {
    let git_dir = path.join(".git");
    if !git_dir.exists() {
        return GitFacts::default();
    }

    let url = fs::read_to_string(git_dir.join("config"))
        .ok()
        .and_then(|config| origin_url(&config));

    let (remote_host, repo_slug) = match url {
        Some(url) => {
            let (host, slug) = parse_remote_url(&url);
            (host, slug)
        }
        None => (None, None),
    };

    GitFacts {
        is_repo: true,
        repo_slug,
        remote_host,
    }
}

/// Extract the url from the `[remote "origin"]` section of a git config
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin {
            if let Some(url) = line.strip_prefix("url") {
                return Some(url.trim_start_matches([' ', '=']).trim().to_string());
            }
        }
    }
    None
}

/// Split a remote URL into (host kind, "owner/repo").
/// Handles `git@host:owner/repo.git` and `scheme://host/owner/repo.git`.
fn parse_remote_url(url: &str) -> (Option<RemoteHost>, Option<String>) {
    let (host, path) = if let Some(rest) = url.split_once('@').map(|(_, r)| r) {
        // git@host:owner/repo.git or user@host/path
        match rest.split_once(':') {
            Some((host, path)) => (host, path),
            None => match rest.split_once('/') {
                Some((host, path)) => (host, path),
                None => return (None, None),
            },
        }
    } else if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        match rest.split_once('/') {
            Some((host, path)) => (host, path),
            None => return (None, None),
        }
    } else {
        return (None, None);
    };

    let host_kind = if host.contains("github") {
        RemoteHost::GitHub
    } else if host.contains("gitlab") {
        RemoteHost::GitLab
    } else if host.contains("gitea") || host.contains("codeberg") {
        RemoteHost::Gitea
    } else {
        RemoteHost::Other
    };

    let slug = path.trim_matches('/').trim_end_matches(".git");
    let slug = (slug.split('/').count() == 2).then(|| slug.to_string());

    (Some(host_kind), slug)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_not_a_repo() {
        let dir = tempdir().unwrap();
        let facts = gather(dir.path());
        assert!(!facts.is_repo);
        assert!(facts.repo_slug.is_none());
    }

    #[test]
    fn test_ssh_remote() {
        let (host, slug) = parse_remote_url("git@github.com:fossable/cibox.git");
        assert_eq!(host, Some(RemoteHost::GitHub));
        assert_eq!(slug.as_deref(), Some("fossable/cibox"));
    }

    #[test]
    fn test_https_remote() {
        let (host, slug) = parse_remote_url("https://gitlab.com/owner/repo");
        assert_eq!(host, Some(RemoteHost::GitLab));
        assert_eq!(slug.as_deref(), Some("owner/repo"));
    }

    #[test]
    fn test_codeberg_maps_to_gitea() {
        let (host, _) = parse_remote_url("https://codeberg.org/owner/repo.git");
        assert_eq!(host, Some(RemoteHost::Gitea));
    }

    #[test]
    fn test_repo_with_origin() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(
            dir.path().join(".git/config"),
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = git@github.com:fossable/cibox.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();
        let facts = gather(dir.path());
        assert!(facts.is_repo);
        assert_eq!(facts.remote_host, Some(RemoteHost::GitHub));
        assert_eq!(facts.repo_slug.as_deref(), Some("fossable/cibox"));
    }
}
