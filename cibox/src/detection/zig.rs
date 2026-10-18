use std::path::Path;

/// Whether the project is a Zig project (build.zig in the root). Zig has no
/// fact beyond its presence: the zig rules take no knobs and read nothing
/// out of the manifest, so there is nothing for a facts struct to hold.
pub(super) fn gather(path: &Path) -> bool {
    path.join("build.zig").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_build_zig() {
        let dir = tempdir().unwrap();
        assert!(!gather(dir.path()));
    }

    #[test]
    fn test_build_zig_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("build.zig"), "pub fn build() void {}\n").unwrap();
        assert!(gather(dir.path()));
    }
}
