use std::fs;
use std::path::Path;

/// Package manager inferred from the lockfile present
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodePackageManager {
    #[default]
    Npm,
    Yarn,
    Pnpm,
    Bun,
}

impl NodePackageManager {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodePackageManager::Npm => "npm",
            NodePackageManager::Yarn => "yarn",
            NodePackageManager::Pnpm => "pnpm",
            NodePackageManager::Bun => "bun",
        }
    }
}

/// Facts about a Node.js project (package.json)
#[derive(Debug, Clone, Default)]
pub struct NodeFacts {
    pub package_manager: NodePackageManager,
    pub has_lockfile: bool,
    pub has_test_script: bool,
    pub has_lint_script: bool,
    pub has_build_script: bool,
    pub has_tsconfig: bool,
    pub has_prettier_config: bool,
    /// `typescript` is a declared dependency, so the install step puts `tsc`
    /// in `node_modules/.bin` at the version the lockfile pins
    pub has_typescript_dep: bool,
    /// `prettier` is a declared dependency, as above
    pub has_prettier_dep: bool,
}

pub(super) fn gather(path: &Path) -> Option<NodeFacts> {
    let contents = fs::read_to_string(path.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&contents).ok()?;

    let script = |name: &str| {
        manifest
            .get("scripts")
            .and_then(|s| s.get(name))
            .and_then(|v| v.as_str())
    };
    // `npm init` seeds a test script that just errors out; treat it as absent
    let has_test_script = script("test").is_some_and(|s| !s.contains("no test specified"));

    let (package_manager, has_lockfile) = package_manager(path);

    Some(NodeFacts {
        package_manager,
        has_lockfile,
        has_test_script,
        has_lint_script: script("lint").is_some(),
        has_build_script: script("build").is_some(),
        has_tsconfig: path.join("tsconfig.json").is_file(),
        has_prettier_config: has_prettier_config(path, &manifest),
        has_typescript_dep: has_dependency(&manifest, "typescript"),
        has_prettier_dep: has_dependency(&manifest, "prettier"),
    })
}

/// Whether `package.json` declares `name` as a dependency of any kind.
///
/// A tool rule can only run a tool the install step actually installs. When
/// the package isn't declared, `npx`/`bunx` would fall back to downloading
/// whatever the registry has under that name — so the rules that run a tool
/// rather than a package.json script gate on this.
fn has_dependency(manifest: &serde_json::Value, name: &str) -> bool {
    [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ]
    .iter()
    .any(|section| {
        manifest
            .get(section)
            .and_then(|deps| deps.get(name))
            .is_some()
    })
}

fn package_manager(path: &Path) -> (NodePackageManager, bool) {
    let lockfiles = [
        ("bun.lockb", NodePackageManager::Bun),
        ("bun.lock", NodePackageManager::Bun),
        ("pnpm-lock.yaml", NodePackageManager::Pnpm),
        ("yarn.lock", NodePackageManager::Yarn),
        ("package-lock.json", NodePackageManager::Npm),
    ];
    for (file, pm) in lockfiles {
        if path.join(file).is_file() {
            return (pm, true);
        }
    }
    (NodePackageManager::Npm, false)
}

fn has_prettier_config(path: &Path, manifest: &serde_json::Value) -> bool {
    if manifest.get("prettier").is_some() {
        return true;
    }
    [
        ".prettierrc",
        ".prettierrc.json",
        ".prettierrc.yml",
        ".prettierrc.yaml",
        ".prettierrc.json5",
        ".prettierrc.js",
        ".prettierrc.cjs",
        ".prettierrc.mjs",
        ".prettierrc.toml",
        "prettier.config.js",
        "prettier.config.cjs",
        "prettier.config.mjs",
    ]
    .iter()
    .any(|f| path.join(f).is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_no_package_json() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_invalid_package_json_is_absent() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{ not json").unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_scripts_detected() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "vitest", "lint": "eslint .", "build": "tsc"}}"#,
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.has_test_script);
        assert!(facts.has_lint_script);
        assert!(facts.has_build_script);
        assert!(!facts.has_tsconfig);
        assert!(!facts.has_prettier_config);
    }

    #[test]
    fn test_placeholder_test_script_counts_as_absent() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )
        .unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(!facts.has_test_script);
    }

    #[test]
    fn test_package_manager_from_lockfile() {
        let cases = [
            ("package-lock.json", NodePackageManager::Npm),
            ("yarn.lock", NodePackageManager::Yarn),
            ("pnpm-lock.yaml", NodePackageManager::Pnpm),
            ("bun.lockb", NodePackageManager::Bun),
            ("bun.lock", NodePackageManager::Bun),
        ];
        for (lockfile, expected) in cases {
            let dir = tempdir().unwrap();
            fs::write(dir.path().join("package.json"), "{}").unwrap();
            fs::write(dir.path().join(lockfile), "").unwrap();
            let facts = gather(dir.path()).unwrap();
            assert_eq!(facts.package_manager, expected, "lockfile {lockfile}");
            assert!(facts.has_lockfile);
        }
    }

    #[test]
    fn test_no_lockfile_defaults_to_npm() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.package_manager, NodePackageManager::Npm);
        assert!(!facts.has_lockfile);
    }

    #[test]
    fn test_bun_wins_over_pnpm() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        fs::write(dir.path().join("bun.lockb"), "").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert_eq!(facts.package_manager, NodePackageManager::Bun);
    }

    #[test]
    fn test_tsconfig_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert!(gather(dir.path()).unwrap().has_tsconfig);
    }

    #[test]
    fn test_prettier_config_detected() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join(".prettierrc"), "{}").unwrap();
        assert!(gather(dir.path()).unwrap().has_prettier_config);

        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("package.json"),
            r#"{"prettier": {"semi": false}}"#,
        )
        .unwrap();
        assert!(gather(dir.path()).unwrap().has_prettier_config);
    }

    #[test]
    fn test_tool_dependencies_detected_in_any_section() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(!facts.has_typescript_dep);
        assert!(!facts.has_prettier_dep);

        for section in [
            "dependencies",
            "devDependencies",
            "optionalDependencies",
            "peerDependencies",
        ] {
            let dir = tempdir().unwrap();
            fs::write(
                dir.path().join("package.json"),
                format!(r#"{{"{section}": {{"typescript": "^5", "prettier": "^3"}}}}"#),
            )
            .unwrap();
            let facts = gather(dir.path()).unwrap();
            assert!(facts.has_typescript_dep, "{section}");
            assert!(facts.has_prettier_dep, "{section}");
        }
    }

    #[test]
    fn test_config_presence_is_not_a_dependency() {
        // A tsconfig.json or .prettierrc is committed by plenty of projects
        // that never install the tool itself
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        fs::write(dir.path().join(".prettierrc"), "{}").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.has_tsconfig);
        assert!(facts.has_prettier_config);
        assert!(!facts.has_typescript_dep);
        assert!(!facts.has_prettier_dep);
    }
}
