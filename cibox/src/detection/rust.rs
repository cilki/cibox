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
    /// The `rust-version` of the root package, if declared
    pub msrv: Option<String>,
    /// The root manifest declares at least one feature
    pub has_features: bool,
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

    let msrv = package
        .and_then(|p| p.rust_version.as_ref())
        .and_then(|v| v.get().ok())
        .cloned();

    Some(RustFacts {
        is_workspace: manifest.workspace.is_some(),
        has_package: package.is_some(),
        publishable,
        package_name: package.map(|p| p.name().to_string()),
        msrv,
        has_features: !manifest.features.is_empty(),
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
    fn test_msrv_from_rust_version() {
        let facts = facts_for(
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nrust-version = \"1.74.0\"\n",
        )
        .unwrap();
        assert_eq!(facts.msrv.as_deref(), Some("1.74.0"));

        let facts = facts_for("[package]\nname = \"lib\"\nversion = \"0.1.0\"\n").unwrap();
        assert_eq!(facts.msrv, None);
    }

    #[test]
    fn test_has_features() {
        let facts = facts_for(
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n\n[features]\nfoo = []\n",
        )
        .unwrap();
        assert!(facts.has_features);

        let facts = facts_for("[package]\nname = \"lib\"\nversion = \"0.1.0\"\n").unwrap();
        assert!(!facts.has_features);
    }

    #[test]
    fn test_workspace_without_root_package() {
        let facts = facts_for("[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        assert!(facts.is_workspace);
        assert!(!facts.has_package);
        assert!(!facts.publishable);
    }
}
