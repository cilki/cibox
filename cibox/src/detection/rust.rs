use cargo_toml::Manifest;
use std::fs;
use std::path::Path;

/// Facts about a Rust project (root Cargo.toml)
#[derive(Debug, Clone, Default)]
pub struct RustFacts {
    pub is_workspace: bool,
    /// The root manifest has a `[package]` section
    pub has_package: bool,
    /// The root package exists and is not `publish = false`
    pub publishable: bool,
    pub package_name: Option<String>,
}

pub(super) fn gather(path: &Path) -> Option<RustFacts> {
    let contents = fs::read_to_string(path.join("Cargo.toml")).ok()?;
    let manifest = Manifest::from_str(&contents).ok()?;

    let package = manifest.package.as_ref();
    let publishable = package.is_some_and(|p| match p.publish.get() {
        Ok(cargo_toml::Publish::Flag(flag)) => *flag,
        Ok(cargo_toml::Publish::Registry(registries)) => !registries.is_empty(),
        // Inherited from the workspace; assume publishable
        Err(_) => true,
    });

    Some(RustFacts {
        is_workspace: manifest.workspace.is_some(),
        has_package: package.is_some(),
        publishable,
        package_name: package.map(|p| p.name().to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts_for(cargo_toml: &str) -> Option<RustFacts> {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();
        gather(dir.path())
    }

    #[test]
    fn test_no_cargo_toml() {
        let dir = tempdir().unwrap();
        assert!(gather(dir.path()).is_none());
    }

    #[test]
    fn test_package_is_publishable() {
        let facts = facts_for("[package]\nname = \"lib\"\nversion = \"0.1.0\"\n").unwrap();
        assert!(facts.has_package);
        assert!(facts.publishable);
        assert!(!facts.is_workspace);
        assert_eq!(facts.package_name.as_deref(), Some("lib"));
    }

    #[test]
    fn test_publish_false_is_not_publishable() {
        let facts =
            facts_for("[package]\nname = \"app\"\nversion = \"0.1.0\"\npublish = false\n")
                .unwrap();
        assert!(!facts.publishable);
    }

    #[test]
    fn test_workspace_without_root_package() {
        let facts = facts_for("[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        assert!(facts.is_workspace);
        assert!(!facts.has_package);
        assert!(!facts.publishable);
    }
}
