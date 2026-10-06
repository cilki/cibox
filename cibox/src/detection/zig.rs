use std::path::Path;

/// Facts about a Zig project (build.zig)
#[derive(Debug, Clone, Default)]
pub struct ZigFacts {
    /// Whether a build.zig.zon package manifest is present
    pub has_zon: bool,
}

pub(super) fn gather(path: &Path) -> Option<ZigFacts> {
    if !path.join("build.zig").is_file() {
        return None;
    }
    Some(ZigFacts {
        has_zon: path.join("build.zig.zon").is_file(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_build_zig() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_build_zig_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("build.zig"), "pub fn build() void {}\n").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(!facts.has_zon);
    }

    #[test]
    fn test_zon_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("build.zig"), "").unwrap();
        fs::write(dir.path().join("build.zig.zon"), ".{}\n").unwrap();
        assert!(gather(dir.path()).unwrap().has_zon);
    }
}
