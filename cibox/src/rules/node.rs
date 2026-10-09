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
            Step::run(
                "Install dependencies",
                if n.yarn_berry {
                    // Modern renamed the flag and errors out on the old one
                    "yarn install --immutable"
                } else {
                    "yarn install --frozen-lockfile"
                },
            ),
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

/// Run a tool the install step installed as part of the project.
///
/// Spelled out rather than run through `npx`/`bunx` on purpose. Both of those
/// fall back to *downloading* a package of that name from the registry and
/// running it when the binary isn't there, which silently puts an unpinned,
/// unreviewed third party in the pipeline — and resolution is by binary name,
/// so `npx tsc` fetches the unrelated `tsc` package rather than `typescript`.
/// A missing binary means the project didn't declare the tool: the job should
/// fail saying so, not reach for the network.
///
/// Yarn Modern defaults to the Plug'n'Play linker, which installs no
/// `node_modules` directory at all, so there its binaries are reached through
/// `yarn exec` — which likewise resolves only within the project and never
/// fetches (that is `yarn dlx`). Everything else lays out `node_modules/.bin`.
fn tool(n: &NodeFacts, cmd: &str) -> String {
    if n.yarn_berry {
        format!("yarn exec {cmd}")
    } else {
        format!("./node_modules/.bin/{cmd}")
    }
}

/// Base job with the per-manager image and a project-relative dependency
/// cache (GitLab caches must live inside the project directory)
fn base_job(id: &str, name: &str, stage: Stage, timeout: u32, n: &NodeFacts) -> Job {
    let (cache_env, cache_path) = match n.package_manager {
        NodePackageManager::Npm => (vec![("npm_config_cache", ".npm-cache")], ".npm-cache/"),
        // Modern ignores cacheFolder entirely while enableGlobalCache is on
        // (its default), and puts the cache under $HOME instead — where no
        // backend can cache it. Turning that off makes YARN_CACHE_FOLDER
        // count again.
        NodePackageManager::Yarn if n.yarn_berry => (
            vec![
                ("YARN_ENABLE_GLOBAL_CACHE", "false"),
                ("YARN_CACHE_FOLDER", ".yarn-cache"),
            ],
            ".yarn-cache/",
        ),
        NodePackageManager::Yarn => (vec![("YARN_CACHE_FOLDER", ".yarn-cache")], ".yarn-cache/"),
        NodePackageManager::Pnpm => (vec![], ".pnpm-store/"),
        NodePackageManager::Bun => (vec![("BUN_INSTALL_CACHE_DIR", ".bun-cache")], ".bun-cache/"),
    };
    let mut job = Job::new(id, name, stage)
        .with_image(image(n))
        .with_timeout(timeout)
        .with_cache("node-cache", vec![cache_path.to_string()]);
    for (key, value) in cache_env {
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
                Step::run("Type-check", tool(&n, "tsc --noEmit")),
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
                Step::run("Check formatting", tool(&n, "prettier --check .")),
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

    const BERRY_PACKAGE_JSON: &str = r#"{
        "packageManager": "yarn@4.6.0",
        "scripts": {"test": "vitest run"},
        "devDependencies": {"typescript": "^5", "prettier": "^3"}
    }"#;

    fn berry_facts() -> ProjectFacts {
        facts_with(&[
            ("package.json", BERRY_PACKAGE_JSON),
            ("yarn.lock", "__metadata:\n  version: 8\n"),
            (".yarnrc.yml", "nodeLinker: pnp\n"),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ])
    }

    fn classic_facts() -> ProjectFacts {
        facts_with(&[
            (
                "package.json",
                r#"{"scripts": {"test": "jest"}, "devDependencies": {"typescript": "^5", "prettier": "^3"}}"#,
            ),
            ("yarn.lock", "# yarn lockfile v1\n"),
            ("tsconfig.json", "{}"),
            (".prettierrc", "{}"),
        ])
    }

    fn commands(facts: &ProjectFacts) -> Vec<String> {
        [
            NodeTest::default().jobs(facts),
            NodeTypecheck.jobs(facts),
            NodeFmt.jobs(facts),
        ]
        .concat()
        .iter()
        .flat_map(|job| job.steps.clone())
        .filter_map(|step| match step {
            Step::Run { command, .. } => Some(command),
            _ => None,
        })
        .collect()
    }

    #[test]
    fn test_yarn_modern_installs_with_immutable() {
        // Modern renamed --frozen-lockfile to --immutable and rejects the old
        // name outright ("Unsupported option name"), so every job in the
        // pipeline died on its install step
        let commands = commands(&berry_facts());
        assert!(
            commands.contains(&"yarn install --immutable".to_string()),
            "{commands:?}"
        );
        assert!(
            !commands.iter().any(|c| c.contains("--frozen-lockfile")),
            "{commands:?}"
        );
    }

    #[test]
    fn test_yarn_classic_keeps_frozen_lockfile() {
        // Classic has no --immutable
        let commands = commands(&classic_facts());
        assert!(
            commands.contains(&"yarn install --frozen-lockfile".to_string()),
            "{commands:?}"
        );
        assert!(
            !commands.iter().any(|c| c.contains("--immutable")),
            "{commands:?}"
        );
    }

    #[test]
    fn test_yarn_modern_runs_tools_through_yarn_exec() {
        // Modern's default Plug'n'Play linker writes no node_modules at all,
        // so the ./node_modules/.bin path is simply not there. `yarn exec`
        // resolves within the project the same way, and still never fetches
        // (that is `yarn dlx`).
        let commands = commands(&berry_facts());
        assert!(
            commands.contains(&"yarn exec tsc --noEmit".to_string()),
            "{commands:?}"
        );
        assert!(
            commands.contains(&"yarn exec prettier --check .".to_string()),
            "{commands:?}"
        );
        for command in &commands {
            assert!(!command.contains("node_modules/.bin"), "{command}");
            assert!(!command.starts_with("yarn dlx"), "{command}");
            assert!(!command.starts_with("npx "), "{command}");
        }
    }

    #[test]
    fn test_yarn_classic_runs_tools_from_node_modules() {
        let commands = commands(&classic_facts());
        assert!(
            commands.contains(&"./node_modules/.bin/tsc --noEmit".to_string()),
            "{commands:?}"
        );
        assert!(
            commands.contains(&"./node_modules/.bin/prettier --check .".to_string()),
            "{commands:?}"
        );
    }

    #[test]
    fn test_yarn_modern_cache_is_not_left_under_home() {
        // Modern ignores cacheFolder while enableGlobalCache is on (its
        // default) and caches under $HOME, outside anything the backends
        // archive — so the cached path would stay empty forever
        let job = &NodeTest::default().jobs(&berry_facts())[0];
        assert_eq!(
            job.cache.as_ref().unwrap().paths,
            vec![".yarn-cache/".to_string()]
        );
        let env = |key: &str| {
            job.env
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(env("YARN_ENABLE_GLOBAL_CACHE"), Some("false"));
        assert_eq!(env("YARN_CACHE_FOLDER"), Some(".yarn-cache"));

        // Classic has no global cache to turn off
        let job = &NodeTest::default().jobs(&classic_facts())[0];
        let keys: Vec<&str> = job.env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["YARN_CACHE_FOLDER"]);
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
