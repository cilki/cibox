use std::fs;
use std::path::Path;

/// Facts about a C/C++ project built with CMake (CMakeLists.txt)
#[derive(Debug, Clone, Default)]
pub struct CmakeFacts {
    /// Whether the root CMakeLists.txt registers tests with ctest
    pub has_tests: bool,
    /// Whether a .clang-format file is present
    pub has_clang_format: bool,
}

pub(super) fn gather(path: &Path) -> Option<CmakeFacts> {
    let contents = fs::read_to_string(path.join("CMakeLists.txt")).ok()?;
    Some(CmakeFacts {
        has_tests: contents.contains("enable_testing") || contents.contains("include(CTest)"),
        has_clang_format: path.join(".clang-format").is_file(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_cmakelists() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_cmakelists_without_tests() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "cmake_minimum_required(VERSION 3.20)\nproject(app)\n",
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(!facts.has_tests);
        assert!(!facts.has_clang_format);
    }

    #[test]
    fn test_enable_testing_detected() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "project(app)\nenable_testing()\nadd_test(NAME t COMMAND t)\n",
        )
        .unwrap();
        assert!(gather(dir.path()).unwrap().has_tests);
    }

    #[test]
    fn test_include_ctest_detected() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("CMakeLists.txt"),
            "project(app)\ninclude(CTest)\n",
        )
        .unwrap();
        assert!(gather(dir.path()).unwrap().has_tests);
    }

    #[test]
    fn test_clang_format_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CMakeLists.txt"), "project(app)\n").unwrap();
        fs::write(dir.path().join(".clang-format"), "BasedOnStyle: LLVM\n").unwrap();
        assert!(gather(dir.path()).unwrap().has_clang_format);
    }
}
