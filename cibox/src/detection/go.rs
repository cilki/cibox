use std::fs;
use std::path::Path;

/// Facts about a Go project (go.mod)
#[derive(Debug, Clone, Default)]
pub struct GoFacts {
    pub module_path: Option<String>,
}

pub(super) fn gather(path: &Path) -> Option<GoFacts> {
    let contents = fs::read_to_string(path.join("go.mod")).ok()?;
    let module_path = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("module "))
        .map(|m| m.trim().to_string());
    Some(GoFacts { module_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_go_mod() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_module_path_parsed() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("go.mod"),
            "module github.com/foo/bar\n\ngo 1.23\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.module_path.as_deref(), Some("github.com/foo/bar"));
    }
}
