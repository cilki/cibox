use super::Rule;
use crate::detection::{NodeFacts, NodePackageManager, ProjectFacts};
use crate::ir::{Job, Stage, Step};

const IMAGE: &str = "node:22";
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

/// Run a devDependency binary with the detected package manager
fn exec_cmd(n: &NodeFacts, cmd: &str) -> String {
    match n.package_manager {
        NodePackageManager::Bun => format!("bunx {cmd}"),
        _ => format!("npx {cmd}"),
    }
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
pub struct NodeTest;

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
        vec![
            base_job(self.id(), self.name(), Stage::Test, 30, &n).with_steps(steps_with_install(
                &n,
                Step::run("Run tests", run_script(&n, "test")),
            )),
        ]
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
        "Type-check TypeScript with tsc --noEmit"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.node.as_ref().is_some_and(|n| n.has_tsconfig)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        vec![
            base_job(self.id(), self.name(), Stage::Lint, 15, &n).with_steps(steps_with_install(
                &n,
                Step::run("Type-check", exec_cmd(&n, "tsc --noEmit")),
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
        "Check formatting with prettier"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.node.as_ref().is_some_and(|n| n.has_prettier_config)
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let n = node(facts);
        vec![
            base_job(self.id(), self.name(), Stage::Lint, 10, &n).with_steps(steps_with_install(
                &n,
                Step::run("Check formatting", exec_cmd(&n, "prettier --check .")),
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
        assert!(NodeTest.detect(&facts));
        assert!(NodeLint.detect(&facts));
        assert!(!NodeTypecheck.detect(&facts));
        assert!(!NodeFmt.detect(&facts));
        assert!(!NodeTest.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_placeholder_test_script_not_detected() {
        let facts = facts_with(&[(
            "package.json",
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )]);
        assert!(!NodeTest.detect(&facts));
    }

    #[test]
    fn test_typecheck_and_fmt_detection() {
        let facts = facts_with(&[
            ("package.json", "{}"),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ]);
        assert!(NodeTypecheck.detect(&facts));
        assert!(NodeFmt.detect(&facts));
    }

    #[test]
    fn test_npm_without_lockfile_uses_npm_install() {
        let jobs = NodeTest.jobs(&ProjectFacts::default());
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
        let jobs = NodeTest.jobs(&facts);
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
        let jobs = NodeTest.jobs(&facts);
        assert_eq!(jobs[0].image.as_deref(), Some("oven/bun:1"));
        assert!(jobs[0].steps.iter().any(|s| matches!(
            s,
            Step::Run { command, .. } if command == "bun install --frozen-lockfile"
        )));
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
            NodeTest.jobs(&ProjectFacts::default())[0].stage,
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
