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
        has_tests: registers_tests(&contents),
        has_clang_format: path.join(".clang-format").is_file(),
    })
}

/// Whether a CMakeLists.txt calls `enable_testing()` or `include(CTest)`.
///
/// CMake command names are case-insensitive (`ENABLE_TESTING()` is as valid
/// as `enable_testing()`, and common in older projects), and a call that is
/// commented out registers nothing, so the source is matched with its
/// comments removed and lowercased.
fn registers_tests(contents: &str) -> bool {
    let code = strip_comments(contents).to_ascii_lowercase();
    let calls = |name: &str| -> Vec<&str> {
        code.match_indices(name)
            .filter(|(at, _)| {
                !code[..*at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            })
            .filter_map(|(at, _)| code[at + name.len()..].trim_start().strip_prefix('('))
            .collect()
    };
    !calls("enable_testing").is_empty()
        || calls("include").into_iter().any(|args| {
            args.trim_start()
                .strip_prefix("ctest")
                .is_some_and(|rest| rest.starts_with(|c: char| c == ')' || c.is_whitespace()))
        })
}

/// `contents` with its `#` line comments and `#[[ ]]` bracket comments
/// blanked out, leaving quoted arguments alone.
fn strip_comments(contents: &str) -> String {
    let mut out = String::with_capacity(contents.len());
    let mut rest = contents;
    while let Some(c) = rest.chars().next() {
        match c {
            '"' => {
                // A quoted argument runs to the next unescaped quote.
                let mut end = rest.len();
                let mut escaped = false;
                for (i, c) in rest.char_indices().skip(1) {
                    match c {
                        _ if escaped => escaped = false,
                        '\\' => escaped = true,
                        '"' => {
                            end = i + 1;
                            break;
                        }
                        _ => {}
                    }
                }
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            }
            '#' => {
                let after = &rest[1..];
                let equals = after
                    .strip_prefix('[')
                    .map(|s| s.len() - s.trim_start_matches('=').len());
                let bracket = equals.filter(|&n| after[1 + n..].starts_with('['));
                let end = match bracket {
                    Some(n) => {
                        let close = format!("]{}]", "=".repeat(n));
                        after[2 + n..]
                            .find(&close)
                            .map_or(rest.len(), |i| 1 + 2 + n + i + close.len())
                    }
                    None => rest.find('\n').unwrap_or(rest.len()),
                };
                out.push(' ');
                rest = &rest[end..];
            }
            _ => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
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
    fn test_command_names_are_case_insensitive() {
        assert!(registers_tests("PROJECT(app)\nENABLE_TESTING()\n"));
        assert!(registers_tests("INCLUDE(CTest)\n"));
        assert!(registers_tests("Include ( CTest )\n"));
        assert!(registers_tests("enable_testing ()\n"));
    }

    #[test]
    fn test_commented_out_calls_register_nothing() {
        assert!(!registers_tests("# enable_testing()\nproject(app)\n"));
        assert!(!registers_tests("project(app) # include(CTest) later\n"));
        assert!(!registers_tests(
            "#[[\nenable_testing()\n]]\n#[==[ include(CTest) ]==]\nproject(app)\n"
        ));
        // Code after a bracket comment is still read.
        assert!(registers_tests("#[[ disabled ]] enable_testing()\n"));
        // A `#` inside a quoted argument is not a comment.
        assert!(registers_tests(
            "message(\"build #1 \\\" done\")\nenable_testing()\n"
        ));
    }

    #[test]
    fn test_lookalikes_register_nothing() {
        assert!(!registers_tests("set(MY_enable_testing ON)\n"));
        assert!(!registers_tests("include(CTestUseLaunchers)\n"));
        assert!(!registers_tests("check_include_file(ctest.h HAVE_CTEST)\n"));
        assert!(!registers_tests(
            "option(BUILD_TESTING \"enable_testing\" ON)\n"
        ));
    }

    #[test]
    fn test_clang_format_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("CMakeLists.txt"), "project(app)\n").unwrap();
        fs::write(dir.path().join(".clang-format"), "BasedOnStyle: LLVM\n").unwrap();
        assert!(gather(dir.path()).unwrap().has_clang_format);
    }
}
