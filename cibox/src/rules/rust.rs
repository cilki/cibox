use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, MatrixEntry, Stage, Step};

/// The official image tracks stable; "rust:stable" is not a docker tag
const IMAGE: &str = "rust:latest";

/// Where cargo keeps its registry/git downloads and `cargo install` binaries.
/// The official images point CARGO_HOME at /usr/local/cargo, which no backend
/// can cache: GitLab rejects cache paths outside the project directory, and
/// the others would need an absolute path that differs per image. Moving it
/// into the checkout makes one project-relative path work everywhere.
const CARGO_HOME: &str = ".cargo";

fn is_rust(facts: &ProjectFacts) -> bool {
    facts.rust.is_some()
}

/// Cache `paths` under `key` and relocate CARGO_HOME, so the `.cargo/` entry
/// among them is the directory cargo actually writes to.
fn with_cargo_cache(job: Job, key: &str, paths: &[&str]) -> Job {
    debug_assert!(
        paths.contains(&".cargo/"),
        "{key}: only jobs caching cargo's home need CARGO_HOME moved"
    );
    job.with_env("CARGO_HOME", CARGO_HOME)
        .with_cache(key, paths.iter().map(|p| p.to_string()).collect())
}

/// The image providing a user-selected toolchain version
fn toolchain_image(version: &str) -> String {
    match version {
        "stable" => IMAGE.to_string(),
        "nightly" => "rustlang/rust:nightly".to_string(),
        v => format!("rust:{v}"),
    }
}

/// Run the test suite with `cargo test`
#[derive(Default)]
pub struct RustTest {
    /// Toolchain versions to matrix over; empty = the default pinned image
    pub versions: Vec<String>,
}

impl Rule for RustTest {
    fn id(&self) -> &'static str {
        "rust-test"
    }

    fn name(&self) -> &'static str {
        "Cargo test"
    }

    fn description(&self) -> &'static str {
        "Run cargo test with all features on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        let job = with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Test).with_timeout(30),
            "rust-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            Step::run("Run tests", "cargo test --all-features"),
        ]);
        vec![match self.versions.as_slice() {
            [] => job.with_image(IMAGE),
            [v] => job.with_image(toolchain_image(v)),
            vs => job.with_matrix(
                vs.iter()
                    .map(|v| MatrixEntry {
                        version: v.clone(),
                        image: toolchain_image(v),
                    })
                    .collect(),
            ),
        }]
    }
}

/// Check formatting with rustfmt
pub struct RustFmt;

impl Rule for RustFmt {
    fn id(&self) -> &'static str {
        "rust-fmt"
    }

    fn name(&self) -> &'static str {
        "Rustfmt"
    }

    fn description(&self) -> &'static str {
        "Check code formatting with cargo fmt"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Lint)
            .with_image(IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout(),
                // The official rust image ships rustup's minimal profile
                Step::run("Install rustfmt", "rustup component add rustfmt"),
                Step::run("Check formatting", "cargo fmt -- --check"),
            ])]
    }
}

/// Lint with clippy, denying warnings
pub struct RustClippy;

impl Rule for RustClippy {
    fn id(&self) -> &'static str {
        "rust-clippy"
    }

    fn name(&self) -> &'static str {
        "Clippy"
    }

    fn description(&self) -> &'static str {
        "Run clippy with warnings denied"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Lint)
                .with_image(IMAGE)
                .with_timeout(15),
            "rust-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            // The official rust image ships rustup's minimal profile
            Step::run("Install clippy", "rustup component add clippy"),
            Step::run("Run clippy", "cargo clippy --all-features -- -D warnings"),
        ])]
    }
}

/// Audit dependencies for known vulnerabilities
pub struct RustAudit;

impl Rule for RustAudit {
    fn id(&self) -> &'static str {
        "rust-audit"
    }

    fn name(&self) -> &'static str {
        "Cargo audit"
    }

    fn description(&self) -> &'static str {
        "Audit dependencies for known vulnerabilities"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Security)
                .with_image(IMAGE)
                .with_timeout(10),
            "cargo-audit-cache",
            &[".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            // Lands in .cargo/bin, which cargo searches for subcommands, so
            // a cache hit turns this into a no-op instead of a rebuild
            Step::run("Install cargo-audit", "cargo install cargo-audit"),
            Step::run("Run audit", "cargo audit"),
        ])]
    }
}

/// Build documentation on nightly with docsrs cfg
pub struct RustDoc;

impl Rule for RustDoc {
    fn id(&self) -> &'static str {
        "rust-doc"
    }

    fn name(&self) -> &'static str {
        "Cargo doc"
    }

    fn description(&self) -> &'static str {
        "Build documentation on nightly with RUSTDOCFLAGS=--cfg docsrs"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        is_rust(facts)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Lint)
                // docsrs cfg (e.g. doc_cfg annotations) needs a nightly
                // toolchain
                .with_image("rustlang/rust:nightly")
                .with_timeout(15)
                .with_env("RUSTDOCFLAGS", "--cfg docsrs"),
            // Nightly artifacts don't mix with the shared stable rust-cache
            "rust-doc-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            Step::run("Build docs", "cargo doc --no-deps --all-features"),
        ])]
    }
}

/// Check the build with the minimum supported rust version
pub struct RustMsrv;

impl Rule for RustMsrv {
    fn id(&self) -> &'static str {
        "rust-msrv"
    }

    fn name(&self) -> &'static str {
        "MSRV check"
    }

    fn description(&self) -> &'static str {
        "Check the build with the rust-version declared in Cargo.toml"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.rust.as_ref().is_some_and(|r| r.msrv.is_some())
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let steps = match facts.rust.as_ref().and_then(|r| r.msrv.as_deref()) {
            Some(msrv) => vec![
                Step::checkout(),
                Step::run(
                    "Install MSRV toolchain",
                    format!("rustup toolchain install {msrv} --profile minimal"),
                ),
                Step::run("Check with MSRV", format!("cargo +{msrv} check")),
            ],
            // Force-enabled without a rust-version fact: derive it in shell
            None => vec![
                Step::checkout(),
                Step::run(
                    "Check with MSRV",
                    "MSRV=$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' Cargo.toml | head -n1) \
                     && rustup toolchain install \"$MSRV\" --profile minimal \
                     && cargo \"+$MSRV\" check",
                ),
            ],
        };
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Lint)
                .with_image(IMAGE)
                .with_timeout(15),
            "rust-msrv-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(steps)]
    }
}

/// Check every feature combination with cargo-hack
pub struct RustFeatureCombos;

impl Rule for RustFeatureCombos {
    fn id(&self) -> &'static str {
        "rust-feature-combos"
    }

    fn name(&self) -> &'static str {
        "Feature combinations"
    }

    fn description(&self) -> &'static str {
        "Check all feature combinations are additive with cargo-hack"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // Additive features matter to downstream crates; binaries pick their
        // own features and can force-enable if wanted
        facts
            .rust
            .as_ref()
            .is_some_and(|r| r.has_features && r.is_library)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Lint)
                .with_image(IMAGE)
                .with_timeout(30),
            "rust-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            // Prebuilt binary; `cargo install` would compile for minutes
            Step::run(
                "Install cargo-hack",
                "curl -fsSL https://github.com/taiki-e/cargo-hack/releases/latest/download/cargo-hack-x86_64-unknown-linux-gnu.tar.gz | tar xz -C /usr/local/bin",
            ),
            Step::run("Check feature powerset", "cargo hack --feature-powerset check"),
        ])]
    }
}

/// Test against the minimal versions of all dependencies
pub struct RustMinimalVersions;

impl Rule for RustMinimalVersions {
    fn id(&self) -> &'static str {
        "rust-minimal-versions"
    }

    fn name(&self) -> &'static str {
        "Minimal versions"
    }

    fn description(&self) -> &'static str {
        "Test with the minimal dependency versions Cargo.toml permits"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // A library-hygiene check: catches version requirements that are
        // lower than what the code actually needs
        facts
            .rust
            .as_ref()
            .is_some_and(|r| r.publishable && r.is_library)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![with_cargo_cache(
            Job::new(self.id(), self.name(), Stage::Test)
                .with_image(IMAGE)
                .with_timeout(30),
            "rust-minimal-cache",
            &["target/", ".cargo/"],
        )
        .with_steps(vec![
            Step::checkout(),
            Step::run(
                "Install nightly for -Zminimal-versions",
                "rustup toolchain install nightly --profile minimal",
            ),
            Step::run(
                "Downgrade to minimal versions",
                "cargo +nightly update -Zminimal-versions",
            ),
            Step::run(
                "Run tests",
                "cargo test --locked --all-features --all-targets",
            ),
        ])]
    }
}

/// Publish to crates.io when a version tag is pushed
pub struct RustRelease;

impl Rule for RustRelease {
    fn id(&self) -> &'static str {
        "rust-release"
    }

    fn name(&self) -> &'static str {
        "Cargo publish"
    }

    fn description(&self) -> &'static str {
        "Publish to crates.io on version tags (requires CARGO_REGISTRY_TOKEN)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // Workspaces without a root package need multi-crate publish
        // ordering, which this rule doesn't attempt; force-enable if wanted
        facts.rust.as_ref().is_some_and(|r| r.publishable)
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Deploy)
            .with_image(IMAGE)
            .with_timeout(15)
            .tags_only()
            .with_secrets(vec!["CARGO_REGISTRY_TOKEN".to_string()])
            .with_steps(vec![
                Step::checkout(),
                Step::run("Publish to crates.io", "cargo publish"),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts(cargo_toml: &str) -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();
        crate::detection::gather_facts(dir.path())
    }

    fn library_facts(cargo_toml: &str) -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), cargo_toml).unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "").unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_rules_detect_any_rust_project() {
        let facts = facts("[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n");
        assert!(RustTest::default().detect(&facts));
        assert!(RustFmt.detect(&facts));
        assert!(RustClippy.detect(&facts));
        assert!(RustAudit.detect(&facts));
        assert!(!RustTest::default().detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_doc_detects_any_rust_project() {
        let facts = facts("[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n");
        assert!(RustDoc.detect(&facts));
        assert!(!RustDoc.detect(&ProjectFacts::default()));

        let jobs = RustDoc.jobs(&facts);
        assert_eq!(jobs[0].id, "rust-doc");
        assert_eq!(jobs[0].stage, Stage::Lint);
        assert!(jobs[0]
            .env
            .contains(&("RUSTDOCFLAGS".to_string(), "--cfg docsrs".to_string())));
    }

    #[test]
    fn test_versions_pin_or_matrix() {
        // Empty = today's default image, no matrix
        let jobs = RustTest::default().jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image.as_deref(), Some("rust:latest"));
        assert_eq!(jobs[0].matrix, None);

        // One version pins the image without a matrix
        let one = RustTest {
            versions: vec!["1.85".to_string()],
        };
        let jobs = one.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image.as_deref(), Some("rust:1.85"));
        assert_eq!(jobs[0].matrix, None);

        // Several versions become matrix entries with mapped images
        let many = RustTest {
            versions: vec![
                "stable".to_string(),
                "nightly".to_string(),
                "1.85".to_string(),
            ],
        };
        let jobs = many.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image, None);
        let entries = jobs[0].matrix.as_ref().unwrap();
        assert_eq!(entries[0].image, "rust:latest");
        assert_eq!(entries[1].image, "rustlang/rust:nightly");
        assert_eq!(entries[2].image, "rust:1.85");
    }

    #[test]
    fn test_msrv_detects_rust_version() {
        let with_msrv = facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nrust-version = \"1.74.0\"\n",
        );
        assert!(RustMsrv.detect(&with_msrv));
        assert!(!RustMsrv.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));

        // The detected version is baked into the commands
        let jobs = RustMsrv.jobs(&with_msrv);
        assert_eq!(jobs[0].id, "rust-msrv");
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "cargo +1.74.0 check")
        ));
    }

    #[test]
    fn test_msrv_force_enabled_derives_version_in_shell() {
        let jobs = RustMsrv.jobs(&ProjectFacts::default());
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. }
                if command.contains("rust-version") && command.contains("$MSRV"))
        ));
    }

    #[test]
    fn test_feature_combos_detects_features_in_libraries() {
        let with_features = library_facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[features]\nfoo = []\n",
        );
        assert!(RustFeatureCombos.detect(&with_features));
        assert!(!RustFeatureCombos.detect(&library_facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));
        // Binary crates don't get the rule by default
        assert!(!RustFeatureCombos.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n\n[features]\nfoo = []\n"
        )));

        let jobs = RustFeatureCombos.jobs(&with_features);
        assert_eq!(jobs[0].id, "rust-feature-combos");
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. }
                if command == "cargo hack --feature-powerset check")
        ));
    }

    #[test]
    fn test_minimal_versions_requires_publishable_library() {
        assert!(RustMinimalVersions.detect(&library_facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));
        assert!(!RustMinimalVersions.detect(&library_facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n"
        )));
        // Binary crates don't get the rule by default
        assert!(!RustMinimalVersions.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));

        let jobs = RustMinimalVersions.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "rust-minimal-versions");
        assert_eq!(jobs[0].stage, Stage::Test);
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. }
                if command == "cargo +nightly update -Zminimal-versions")
        ));
    }

    #[test]
    fn test_release_requires_publishable_package() {
        assert!(RustRelease.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n"
        )));
        assert!(!RustRelease.detect(&facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\npublish = false\n"
        )));
        assert!(!RustRelease.detect(&facts("[workspace]\nmembers = []\n")));
    }

    #[test]
    fn test_release_job_is_tags_only_with_token() {
        let jobs = RustRelease.jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "rust-release");
        assert!(jobs[0].tags_only);
        assert_eq!(jobs[0].stage, Stage::Deploy);
        assert_eq!(jobs[0].secrets, vec!["CARGO_REGISTRY_TOKEN"]);
        assert!(jobs[0].steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command == "cargo publish")
        ));
    }

    /// Every rust rule, with facts rich enough that each one emits its job
    fn all_rust_jobs() -> Vec<Job> {
        let facts = library_facts(
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nrust-version = \"1.74.0\"\n\n\
             [features]\nfoo = []\n",
        );
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(RustTest::default()),
            Box::new(RustFmt),
            Box::new(RustClippy),
            Box::new(RustAudit),
            Box::new(RustDoc),
            Box::new(RustMsrv),
            Box::new(RustFeatureCombos),
            Box::new(RustMinimalVersions),
            Box::new(RustRelease),
        ];
        for rule in &rules {
            assert!(rule.detect(&facts), "{} should have detected", rule.id());
        }
        rules.iter().flat_map(|r| r.jobs(&facts)).collect()
    }

    /// `.cargo/` is only worth caching if cargo writes there: the official
    /// images keep CARGO_HOME at /usr/local/cargo, so without the env the
    /// backends archive a directory that never exists.
    #[test]
    fn test_cached_cargo_home_is_the_one_cargo_uses() {
        let jobs = all_rust_jobs();
        assert!(jobs.iter().any(|j| j.id == "rust-test"));
        for job in &jobs {
            let caches_cargo_home = job
                .cache
                .as_ref()
                .is_some_and(|c| c.paths.iter().any(|p| p == ".cargo/"));
            let moves_cargo_home = job
                .env
                .iter()
                .any(|(k, v)| k == "CARGO_HOME" && v == ".cargo");
            assert_eq!(
                caches_cargo_home, moves_cargo_home,
                "{}: caching .cargo/ and setting CARGO_HOME must go together",
                job.id
            );
        }
    }

    /// GitLab refuses cache paths outside the project directory, so the IR
    /// may only ever name relative ones
    #[test]
    fn test_cache_paths_are_project_relative() {
        for job in all_rust_jobs() {
            for path in job.cache.iter().flat_map(|c| &c.paths) {
                assert!(
                    !path.starts_with('/'),
                    "{}: cache path {path} is outside the project directory",
                    job.id
                );
            }
        }
    }

    #[test]
    fn test_test_job_shape() {
        let jobs = RustTest::default().jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "rust-test");
        assert_eq!(jobs[0].image.as_deref(), Some("rust:latest"));
        assert!(jobs[0].cache.is_some());
        assert!(!jobs[0].tags_only);
    }
}
