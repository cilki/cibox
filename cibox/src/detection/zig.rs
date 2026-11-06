use std::fs;
use std::path::Path;

/// Facts about a Zig project (build.zig)
#[derive(Debug, Clone, Default)]
pub struct ZigFacts {
    /// Whether a build.zig.zon package manifest is present
    pub has_zon: bool,
    /// Whether build.zig declares a `test` step for `zig build test` to run
    pub has_test_step: bool,
}

pub(super) fn gather(path: &Path) -> Option<ZigFacts> {
    let build = path.join("build.zig");
    if !build.is_file() {
        return None;
    }
    Some(ZigFacts {
        has_zon: path.join("build.zig.zon").is_file(),
        has_test_step: fs::read_to_string(&build).is_ok_and(|s| declares_test_step(&s)),
    })
}

/// Whether a build script registers a top-level step named `test`.
///
/// `zig build test` only works when the script declares one — `zig init`'s
/// template does, but nothing makes a project keep it, and without it the
/// command fails with "no step named 'test'". Steps are declared with
/// `b.step("test", ...)`, whatever the builder is called and however the call
/// is wrapped, so whitespace is ignored when matching.
fn declares_test_step(build_zig: &str) -> bool {
    let compact: String = build_zig.split_whitespace().collect();
    compact.contains(".step(\"test\"")
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
        assert!(!facts.has_test_step);
    }

    #[test]
    fn test_zon_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("build.zig"), "").unwrap();
        fs::write(dir.path().join("build.zig.zon"), ".{}\n").unwrap();
        assert!(gather(dir.path()).unwrap().has_zon);
    }

    #[test]
    fn test_test_step_detected() {
        // As `zig init` writes it
        assert!(declares_test_step(
            "    const test_step = b.step(\"test\", \"Run unit tests\");\n"
        ));
        // A differently named builder, the call wrapped over lines
        assert!(declares_test_step(
            "const t = builder.step(\n    \"test\",\n    \"Run tests\",\n);\n"
        ));
        // Other steps, or a test artifact with no step to run it, don't count
        assert!(!declares_test_step(
            "const run_step = b.step(\"run\", \"Run the app\");\n\
             const tests = b.addTest(.{ .root_module = mod });\n\
             const s = b.step(\"test-integration\", \"\");\n"
        ));
    }
}
