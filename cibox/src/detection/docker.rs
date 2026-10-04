use std::path::Path;

/// Facts about a Docker build (a Dockerfile in the project root)
#[derive(Debug, Clone)]
pub struct DockerFacts {
    /// Filename of the Dockerfile found (e.g. "Dockerfile")
    pub dockerfile: String,
}

pub(super) fn gather(path: &Path) -> Option<DockerFacts> {
    ["Dockerfile", "dockerfile", "Containerfile"]
        .iter()
        .find(|name| path.join(name).is_file())
        .map(|name| DockerFacts {
            dockerfile: name.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_dockerfile() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_dockerfile_found() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.dockerfile, "Dockerfile");
    }
}
