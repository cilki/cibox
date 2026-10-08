use super::{with_versions, Rule};
use crate::detection::{NodeFacts, NodePackageManager, ProjectFacts};
use crate::ir::{Job, Stage, Step};

pub(crate) const IMAGE: &str = "node:22";
const BUN_IMAGE: &str = "oven/bun:1";

/// Facts to build jobs from; force-enabled rules fall back to npm without a
/// lockfile
fn node(facts: &ProjectFacts) -> NodeFacts {
    facts.node.clone().unwrap_or_default()
}

fn image(n: &NodeFacts) -> &'static str {
    match n.package_manager {
        NodePackageManager::Bun => BUN_IMAGE,
        _ => IMAGE,
    }
}

fn install_steps(n: &NodeFacts) -> Vec<Step> {
    match n.package_manager {
        NodePackageManager::Npm => vec![Step::run(
            "Install dependencies",
            if n.has_lockfile {
                "npm ci"
            } else {
                "npm install"
            },
        )],
        NodePackageManager::Yarn => vec![
            Step::run("Enable corepack", "corepack enable"),
            Step::run("Install dependencies", "yarn install --frozen-lockfile"),
        ],
        NodePackageManager::Pnpm => vec![
            Step::run("Enable corepack", "corepack enable"),
            Step::run(
                "Install dependencies",
                "pnpm install --frozen-lockfile --store-dir .pnpm-store",
            ),
        ],
        NodePackageManager::Bun => vec![Step::run(
            "Install dependencies",
            "bun install --frozen-lockfile",
        )],
    }
}

/// Run a package.json script with the detected package manager
fn run_script(n: &NodeFacts, script: &str) -> String {
    match n.package_manager {
        NodePackageManager::Npm => format!("npm run {script}"),
        NodePackageManager::Yarn => format!("yarn run {script}"),
        NodePackageManager::Pnpm => format!("pnpm run {script}"),
        NodePackageManager::Bun => format!("bun run {script}"),
    }
}

/// Run a tool the install step put in the project's `node_modules/.bin`.
///
/// Spelled as a path rather than through `npx`/`bunx` on purpose. Both of
/// those fall back to *downloading* a package of that name from the registry
/// and running it when the binary isn't there, which silently puts an
/// unpinned, unreviewed third party in the pipeline — and resolution is by
/// binary name, so `npx tsc` fetches the unrelated `tsc` package rather than
/// `typescript`. Every package manager cibox emits for installs binaries
/// here, so a missing one means the project didn't declare the tool: the job
/// should fail saying so, not reach for the network.
fn local_bin(cmd: &str) -> String {
    format!("./node_modules/.bin/{cmd}")
}

/// Base job with the per-manager image and a project-relative dependency
/// cache (GitLab caches must live inside the project directory)
fn base_job(id: &str, name: &str, stage: Stage, timeout: u32, n: &NodeFacts) -> Job {
    let (cache_env, cache_path) = match n.package_manager {
        NodePackageManager::Npm => (Some(("npm_config_cache", ".npm-cache")), ".npm-cache/"),
        NodePackageManager::Yarn => (Some(("YARN_CACHE_FOLDER", ".yarn-cache")), ".yarn-cache/"),
        NodePackageManager::Pnpm => (None, ".pnpm-store/"),
        NodePackageManager::Bun => (Some(("BUN_INSTALL_CACHE_DIR", ".bun-cache")), ".bun-cache/"),
    };
    let mut job = Job::new(id, name, stage)
        .with_image(image(n))
        .with_timeout(timeout)
        .with_cache("node-cache", vec![cache_path.to_string()]);
    if let Some((key, value)) = cache_env {
        job = job.with_env(key, value);
    }
    job
}

fn steps_with_install(n: &NodeFacts, final_step: Step) -> Vec<Step> {
    let mut steps = vec![Step::checkout()];
    steps.extend(install_steps(n));
    steps.push(final_step);
    steps
}

/// Run the package.json test script
#[derive(Default)]
pub struct NodeTest {
    /// Toolchain versions to matrix over; empty = the default pinned image.
    /// Ignored for bun projects, whose toolchain comes with the bun image.
    pub versions: Vec<String>,
}

impl Rule for NodeTest {
    fn id(&self) -> &'static str {
        "node-test"
    }

    fn name(&self) -> &'static str {
        "Node test"
    }

    fn description(&self) -> &'static str {
        "Install dependencies and run the package.json test script"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.node.as_ref().is_some_and(|n| n.has_test_script)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        let job = base_job(self.id(), self.name(), Stage::Test, 30, &n).with_steps(
            steps_with_install(&n, Step::run("Run tests", run_script(&n, "test"))),
        );
        // The bun toolchain comes with the bun image, so the knob can't apply
        let versions: &[String] = match n.package_manager {
            NodePackageManager::Bun => &[],
            _ => &self.versions,
        };
        vec![with_versions(job, versions, |v| format!("node:{v}"))]
    }
}

/// Run the package.json lint script
pub struct NodeLint;

impl Rule for NodeLint {
    fn id(&self) -> &'static str {
        "node-lint"
    }

    fn name(&self) -> &'static str {
        "Node lint"
    }

    fn description(&self) -> &'static str {
        "Run the package.json lint script"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.node.as_ref().is_some_and(|n| n.has_lint_script)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        vec![
            base_job(self.id(), self.name(), Stage::Lint, 15, &n).with_steps(steps_with_install(
                &n,
                Step::run("Run lint", run_script(&n, "lint")),
            )),
        ]
    }
}

/// Type-check TypeScript with tsc
pub struct NodeTypecheck;

impl Rule for NodeTypecheck {
    fn id(&self) -> &'static str {
        "node-typecheck"
    }

    fn name(&self) -> &'static str {
        "TypeScript check"
    }

    fn description(&self) -> &'static str {
        "Type-check TypeScript with tsc --noEmit (requires a typescript dependency)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // A tsconfig.json on its own doesn't mean the project installs a
        // compiler — plenty are committed only for an editor, or for a
        // bundler that carries its own. Running `tsc` anyway would mean
        // fetching one, so leave the rule to be force-enabled instead.
        facts
            .node
            .as_ref()
            .is_some_and(|n| n.has_tsconfig && n.has_typescript_dep)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        vec![
            base_job(self.id(), self.name(), Stage::Lint, 15, &n).with_steps(steps_with_install(
                &n,
                Step::run("Type-check", local_bin("tsc --noEmit")),
            )),
        ]
    }
}

/// Check formatting with prettier
pub struct NodeFmt;

impl Rule for NodeFmt {
    fn id(&self) -> &'static str {
        "node-fmt"
    }

    fn name(&self) -> &'static str {
        "Prettier"
    }

    fn description(&self) -> &'static str {
        "Check formatting with prettier (requires a prettier dependency)"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        // A config file alone says which style the project uses, not that it
        // installs the formatter; see NodeTypecheck's note above
        facts
            .node
            .as_ref()
            .is_some_and(|n| n.has_prettier_config && n.has_prettier_dep)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        vec![
            base_job(self.id(), self.name(), Stage::Lint, 10, &n).with_steps(steps_with_install(
                &n,
                Step::run("Check formatting", local_bin("prettier --check .")),
            )),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn facts_with(files: &[(&str, &str)]) -> ProjectFacts {
        let dir = tempdir().unwrap();
        for (name, contents) in files {
            fs::write(dir.path().join(name), contents).unwrap();
        }
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_detect_requires_scripts() {
        let facts = facts_with(&[(
            "package.json",
            r#"{"scripts": {"test": "vitest", "lint": "eslint ."}}"#,
        )]);
        assert!(NodeTest::default().detect(&facts));
        assert!(NodeLint.detect(&facts));
        assert!(!NodeTypecheck.detect(&facts));
        assert!(!NodeFmt.detect(&facts));
        assert!(!NodeTest::default().detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_placeholder_test_script_not_detected() {
        let facts = facts_with(&[(
            "package.json",
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )]);
        assert!(!NodeTest::default().detect(&facts));
    }

    #[test]
    fn test_typecheck_and_fmt_detection() {
        let facts = facts_with(&[
            (
                "package.json",
                r#"{"devDependencies": {"typescript": "^5", "prettier": "^3"}}"#,
            ),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ]);
        assert!(NodeTypecheck.detect(&facts));
        assert!(NodeFmt.detect(&facts));
    }

    #[test]
    fn test_tool_rules_need_the_tool_installed() {
        // A config file says which style/compiler settings the project uses,
        // not that it installs the tool. Enabling the rule anyway would make
        // the job download one at an unpinned version nobody reviewed.
        let facts = facts_with(&[
            ("package.json", "{}"),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ]);
        assert!(!NodeTypecheck.detect(&facts));
        assert!(!NodeFmt.detect(&facts));

        // One declared tool doesn't enable the other's rule
        let facts = facts_with(&[
            ("package.json", r#"{"dependencies": {"typescript": "^5"}}"#),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ]);
        assert!(NodeTypecheck.detect(&facts));
        assert!(!NodeFmt.detect(&facts));
    }

    #[test]
    fn test_tools_run_from_node_modules_not_the_registry() {
        // `npx tsc` / `bunx prettier` download and run a package of that name
        // when the binary is missing — and `tsc` on npm is not the TypeScript
        // compiler. The install step is the only thing allowed to fetch.
        for (lockfile, pm) in [("package-lock.json", "npm"), ("bun.lockb", "bun")] {
            let facts = facts_with(&[
                (
                    "package.json",
                    r#"{"devDependencies": {"typescript": "^5", "prettier": "^3"}}"#,
                ),
                ("tsconfig.json", "{}"),
                (".prettierrc", "{}"),
                (lockfile, ""),
            ]);
            let commands: Vec<String> = [NodeTypecheck.jobs(&facts), NodeFmt.jobs(&facts)]
                .concat()
                .iter()
                .flat_map(|job| job.steps.clone())
                .filter_map(|step| match step {
                    Step::Run { command, .. } => Some(command),
                    _ => None,
                })
                .collect();

            assert!(
                commands.contains(&"./node_modules/.bin/tsc --noEmit".to_string()),
                "{pm}: {commands:?}"
            );
            assert!(
                commands.contains(&"./node_modules/.bin/prettier --check .".to_string()),
                "{pm}: {commands:?}"
            );
            for command in &commands {
                assert!(
                    !command.starts_with("npx ") && !command.starts_with("bunx "),
                    "{pm}: {command}"
                );
            }
        }
    }

    #[test]
    fn test_npm_without_lockfile_uses_npm_install() {
        let jobs = NodeTest::default().jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].id, "node-test");
        assert_eq!(jobs[0].image.as_deref(), Some("node:22"));
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "npm install"
        )));
    }

    #[test]
    fn test_npm_with_lockfile_uses_npm_ci() {
        let facts = facts_with(&[
            ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
            ("package-lock.json", "{}"),
        ]);
        let jobs = NodeTest::default().jobs(&facts);
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "npm ci"
        )));
    }

    #[test]
    fn test_bun_switches_image() {
        let facts = facts_with(&[
            ("package.json", r#"{"scripts": {"test": "bun test"}}"#),
            ("bun.lockb", ""),
        ]);
        let jobs = NodeTest::default().jobs(&facts);
        assert_eq!(jobs[0].image.as_deref(), Some("oven/bun:1"));
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "bun install --frozen-lockfile"
        )));
    }

    #[test]
    fn test_versions_pin_or_matrix() {
        let one = NodeTest {
            versions: vec!["20".to_string()],
        };
        let jobs = one.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image.as_deref(), Some("node:20"));
        assert_eq!(jobs[0].matrix, None);

        let two = NodeTest {
            versions: vec!["20".to_string(), "22".to_string()],
        };
        let jobs = two.jobs(&ProjectFacts::default());
        assert_eq!(jobs[0].image, None);
        let entries = jobs[0].matrix.as_ref().unwrap();
        assert_eq!(entries[0].image, "node:20");
        assert_eq!(entries[1].image, "node:22");
    }

    #[test]
    fn test_versions_ignored_for_bun() {
        let facts = facts_with(&[
            ("package.json", r#"{"scripts": {"test": "bun test"}}"#),
            ("bun.lockb", ""),
        ]);
        let rule = NodeTest {
            versions: vec!["20".to_string(), "22".to_string()],
        };
        let jobs = rule.jobs(&facts);
        assert_eq!(jobs[0].image.as_deref(), Some("oven/bun:1"));
        assert_eq!(jobs[0].matrix, None);
    }

    #[test]
    fn test_pnpm_enables_corepack() {
        let facts = facts_with(&[("package.json", "{}"), ("pnpm-lock.yaml", "")]);
        let jobs = NodeLint.jobs(&facts);
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "corepack enable"
        )));
    }

    #[test]
    fn test_job_stages() {
        assert_eq!(
            NodeTest::default().jobs(&ProjectFacts::default())[0].stage,
            Stage::Test
        );
        assert_eq!(
            NodeLint.jobs(&ProjectFacts::default())[0].stage,
            Stage::Lint
        );
        assert_eq!(
            NodeTypecheck.jobs(&ProjectFacts::default())[0].stage,
            Stage::Lint
        );
        assert_eq!(NodeFmt.jobs(&ProjectFacts::default())[0].stage, Stage::Lint);
    }
}
