use std::fs;
use std::path::{Path, PathBuf};

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

/// Read git facts straight from the repository's config — no git subprocess
/// needed
pub(super) fn gather(path: &Path) -> GitFacts {
    if !path.join(".git").exists() {
        return GitFacts::default();
    }

    let url = config_dir(path)
        .and_then(|dir| fs::read_to_string(dir.join("config")).ok())
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

/// The directory holding the repository's `config`. Usually `<path>/.git`,
/// but `.git` is a *file* holding `gitdir: <path>` in a linked worktree or a
/// submodule, and a linked worktree's own git dir keeps no config — it shares
/// the main one's, named by its `commondir`. Reading `<path>/.git/config`
/// blindly finds nothing in either case, which silently costs the remote and
/// everything derived from it (platform inference, the default image and AUR
/// package names).
fn config_dir(path: &Path) -> Option<PathBuf> {
    let dot_git = path.join(".git");
    let git_dir = if fs::metadata(&dot_git).ok()?.is_dir() {
        dot_git
    } else {
        let pointer = fs::read_to_string(&dot_git).ok()?;
        let target = pointer
            .lines()
            .find_map(|line| line.trim().strip_prefix("gitdir:"))?;
        resolve_against(path, target.trim())
    };

    match fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => Some(resolve_against(&git_dir, common.trim())),
        Err(_) => Some(git_dir),
    }
}

/// Git's pointer files hold either an absolute path or one relative to the
/// file's own directory
fn resolve_against(base: &Path, target: &str) -> PathBuf {
    let target = Path::new(target);
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        base.join(target)
    }
}

/// Extract the url from the `[remote "origin"]` section of a git config
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix('[') {
            let header = rest.split_once(']').map_or(rest, |(header, _)| header);
            // Section names are case-insensitive, subsection names are not
            let (section, subsection) = match header.trim().split_once(char::is_whitespace) {
                Some((section, subsection)) => (section, subsection.trim()),
                None => (header.trim(), ""),
            };
            in_origin = section.eq_ignore_ascii_case("remote") && subsection == "\"origin\"";
        } else if in_origin {
            // Key names are case-insensitive too, and `url` has to match the
            // whole of one: a prefix match also answers to `urlsomething`
            if let Some((key, value)) = line.split_once('=') {
                if key.trim().eq_ignore_ascii_case("url") {
                    return Some(unquote_value(value));
                }
            }
        }
    }
    None
}

/// Read a git config value the way git does: a `"` toggles quoting rather
/// than being part of the value, and `\` escapes the next character.
/// go-git, libgit2 and JGit all write `url = "https://..."`, so taking the
/// text verbatim leaves the closing quote stuck on the end of the slug —
/// enough to stop `.git` being stripped and to mangle the derived names.
fn unquote_value(value: &str) -> String {
    // Whitespace around the value is not part of it; whitespace *inside* the
    // quotes is, and the quotes keep it out of this trim
    let value = value.trim();
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {}
            // Only git's own escapes mean anything; anything else is kept
            // verbatim rather than silently swallowing the backslash
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{8}'),
                Some(escaped) => out.push(escaped),
                None => out.push('\\'),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Split a remote URL into (host kind, "owner/repo").
///
/// Git takes two shapes, and `:` means something different in each: in
/// `scheme://[user[:password]@]host[:port]/owner/repo[.git]` it introduces a
/// *port*, while in the scp-like `[user@]host:owner/repo[.git]` it introduces
/// the *path*. Only the scheme tells them apart, so it has to be read first —
/// taking `ssh://git@host:2222/owner/repo.git` for the scp shape makes
/// `2222/owner/repo` the path, which is no slug at all, and the default image
/// and AUR package names derived from it fall back to the local directory
/// name without a word. A self-hosted forge reachable on a non-default SSH
/// port is exactly the URL that happens to.
fn parse_remote_url(url: &str) -> (Option<RemoteHost>, Option<String>) {
    let split = match url.split_once("://") {
        // With a scheme the authority runs up to the first `/`
        Some((_, rest)) => rest.split_once('/'),
        // Without one this is scp-like, but only while the `:` comes before
        // any `/`; otherwise the url is a plain local path
        None => url
            .split_once(':')
            .filter(|(authority, _)| !authority.contains('/')),
    };
    let Some((authority, path)) = split else {
        return (None, None);
    };

    // `user[:password]@` is credentials rather than part of the host name...
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // ...and nor is a trailing `:port`
    let host = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
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

    /// A port is part of the authority, not of the path: a self-hosted forge
    /// on a non-default SSH port still has a slug. Gitea's own compose file
    /// publishes 2222, so this is the url a self-hosted Gitea hands out.
    #[test]
    fn test_ssh_url_with_an_explicit_port() {
        let (host, slug) = parse_remote_url("ssh://git@gitea.example.com:2222/acme/widget.git");
        assert_eq!(host, Some(RemoteHost::Gitea));
        assert_eq!(slug.as_deref(), Some("acme/widget"));

        let (host, slug) = parse_remote_url("https://gitlab.example.com:8443/acme/widget");
        assert_eq!(host, Some(RemoteHost::GitLab));
        assert_eq!(slug.as_deref(), Some("acme/widget"));
    }

    /// Credentials in the authority are not part of the host name either
    #[test]
    fn test_url_with_credentials() {
        let (host, slug) = parse_remote_url("https://oauth2:t0ken@gitlab.com/owner/repo.git");
        assert_eq!(host, Some(RemoteHost::GitLab));
        assert_eq!(slug.as_deref(), Some("owner/repo"));

        let (host, slug) = parse_remote_url("ssh://git@github.com/owner/repo.git");
        assert_eq!(host, Some(RemoteHost::GitHub));
        assert_eq!(slug.as_deref(), Some("owner/repo"));
    }

    /// The scp-like shape carries no user when the remote is configured for
    /// one in `~/.ssh/config`
    #[test]
    fn test_scp_like_url_without_a_user() {
        let (host, slug) = parse_remote_url("github.com:fossable/cibox.git");
        assert_eq!(host, Some(RemoteHost::GitHub));
        assert_eq!(slug.as_deref(), Some("fossable/cibox"));
    }

    /// A url with no host and path to split — a local clone, or a forge url
    /// missing the repository — leaves both facts absent rather than guessing
    #[test]
    fn test_remote_without_a_host_and_a_path() {
        assert_eq!(parse_remote_url("/srv/git/repo.git"), (None, None));
        assert_eq!(parse_remote_url("../sibling/repo.git"), (None, None));
        assert_eq!(parse_remote_url("https://gitlab.com"), (None, None));
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

    /// Write a conventional `.git` directory holding `config`
    fn git_dir(root: &Path, config: &str) -> PathBuf {
        let git_dir = root.join(".git");
        fs::create_dir_all(&git_dir).unwrap();
        fs::write(git_dir.join("config"), config).unwrap();
        git_dir
    }

    fn origin(url: &str) -> String {
        format!("[remote \"origin\"]\n\turl = {url}\n")
    }

    /// go-git, libgit2 and JGit write the url quoted; the quotes are syntax,
    /// not part of the value
    #[test]
    fn test_quoted_config_values() {
        assert_eq!(
            origin_url(&origin("\"git@github.com:fossable/cibox.git\"")).as_deref(),
            Some("git@github.com:fossable/cibox.git")
        );
        let (host, slug) =
            parse_remote_url(&origin_url(&origin("\"https://git.cilki.net/cilki/cibox\"")).unwrap());
        assert_eq!(host, Some(RemoteHost::Other));
        assert_eq!(slug.as_deref(), Some("cilki/cibox"));
    }

    #[test]
    fn test_escapes_in_a_quoted_value() {
        assert_eq!(
            origin_url(&origin(r#""https://host/o/say-\"hi\"""#)).as_deref(),
            Some(r#"https://host/o/say-"hi""#)
        );
    }

    /// Only the quotes bound whitespace that belongs to the value
    #[test]
    fn test_whitespace_around_a_value_is_dropped_but_not_inside_it() {
        assert_eq!(
            origin_url("[remote \"origin\"]\n\turl =   \" a/b \"  \n").as_deref(),
            Some(" a/b ")
        );
    }

    /// Section and key names are case-insensitive, and `url` has to be the
    /// whole key — not just its prefix
    #[test]
    fn test_key_and_section_names_are_matched_the_way_git_matches_them() {
        assert_eq!(
            origin_url("[REMOTE \"origin\"]\n\tURL = https://host/o/r\n").as_deref(),
            Some("https://host/o/r")
        );
        assert_eq!(
            origin_url("[remote \"origin\"]\n\turlsomething = nope\n\turl = https://host/o/r\n")
                .as_deref(),
            Some("https://host/o/r")
        );
        // A different remote's url is not origin's
        assert_eq!(
            origin_url("[remote \"upstream\"]\n\turl = https://host/o/r\n"),
            None
        );
    }

    /// `git worktree add` leaves a `.git` file pointing at a git dir that
    /// holds no config of its own — the shared one is named by `commondir`
    #[test]
    fn test_linked_worktree_finds_the_shared_config() {
        let dir = tempdir().unwrap();
        let main = dir.path().join("main");
        fs::create_dir_all(&main).unwrap();
        let git_dir = git_dir(&main, &origin("git@github.com:fossable/cibox.git"));

        let worktree_git = git_dir.join("worktrees/feature");
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(worktree_git.join("commondir"), "../..\n").unwrap();

        let worktree = dir.path().join("feature");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();

        let facts = gather(&worktree);
        assert!(facts.is_repo);
        assert_eq!(facts.remote_host, Some(RemoteHost::GitHub));
        assert_eq!(facts.repo_slug.as_deref(), Some("fossable/cibox"));
    }

    /// A submodule's `.git` file points straight at a git dir with a config,
    /// by a path relative to the submodule's own directory
    #[test]
    fn test_submodule_follows_a_relative_gitdir() {
        let dir = tempdir().unwrap();
        let super_git = git_dir(dir.path(), "[core]\n\tbare = false\n");
        let module_git = super_git.join("modules/sub");
        fs::create_dir_all(&module_git).unwrap();
        fs::write(
            module_git.join("config"),
            origin("https://gitlab.com/owner/sub.git"),
        )
        .unwrap();

        let sub = dir.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join(".git"), "gitdir: ../.git/modules/sub\n").unwrap();

        let facts = gather(&sub);
        assert!(facts.is_repo);
        assert_eq!(facts.remote_host, Some(RemoteHost::GitLab));
        assert_eq!(facts.repo_slug.as_deref(), Some("owner/sub"));
    }

    /// A `.git` file that points nowhere useful still marks a repository;
    /// detection never fails, the facts behind it are just absent
    #[test]
    fn test_dangling_gitdir_pointer_leaves_the_remote_absent() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join(".git"), "gitdir: ./nowhere\n").unwrap();
        let facts = gather(dir.path());
        assert!(facts.is_repo);
        assert!(facts.repo_slug.is_none());
        assert!(facts.remote_host.is_none());
    }
}
