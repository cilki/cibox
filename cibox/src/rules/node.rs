use super::{with_versions, Rule};
use crate::detection::{NodeFacts, NodePackageManager, ProjectFacts};
use crate::ir::{Job, Stage, Step};

/// The `node:N` images to choose from, oldest first
const IMAGES: [(u64, &str); 5] = [
    (16, "node:16"),
    (18, "node:18"),
    (20, "node:20"),
    (22, "node:22"),
    (24, "node:24"),
];
/// The major used whenever the project doesn't rule it out
const DEFAULT_MAJOR: u64 = 22;
const BUN_IMAGE: &str = "oven/bun:1";

/// Facts to build jobs from; force-enabled rules fall back to npm without a
/// lockfile
fn node(facts: &ProjectFacts) -> NodeFacts {
    facts.node.clone().unwrap_or_default()
}

/// The image the node jobs run on when no versions are configured
pub(crate) fn default_image(facts: &ProjectFacts) -> &'static str {
    image(&node(facts))
}

/// The image for a project's jobs.
///
/// pnpm and Yarn Classic refuse to install a project whose own
/// `engines.node` excludes the running Node (npm only warns, unless
/// `engine-strict` is set), so a fixed default fails every job's install step
/// for a project pinned to an older line (`"engines": {"node": "20.x"}`) or
/// needing a newer one (`>=24`). The default stands whenever the project
/// allows it; otherwise the nearest major it does allow is used, the newer on
/// a tie. A range this can't read, or one no image satisfies, keeps the
/// default — the package manager will report it more clearly than a guess
/// would.
fn image(n: &NodeFacts) -> &'static str {
    if n.package_manager == NodePackageManager::Bun {
        return BUN_IMAGE;
    }
    let default = || {
        IMAGES
            .iter()
            .find(|(major, _)| *major == DEFAULT_MAJOR)
            .map(|(_, image)| *image)
            .expect("the default major has an image")
    };
    let Some(range) = n.engines_node.as_deref() else {
        return default();
    };
    let mut nearest = IMAGES.to_vec();
    nearest.sort_by_key(|&(major, _)| (major.abs_diff(DEFAULT_MAJOR), std::cmp::Reverse(major)));
    nearest
        .into_iter()
        .map(|(major, image)| allows(range, major).map(|ok| ok.then_some(image)))
        .collect::<Option<Vec<_>>>()
        .and_then(|admitted| admitted.into_iter().flatten().next())
        .unwrap_or_else(default)
}

type Version = [u64; 3];

/// One side of an interval: the version, and a flag ordered so that the
/// tighter of two bounds at the same version compares as the narrower one —
/// "excluded" for a lower bound (taken with `max`), "included" for an upper
/// bound (taken with `min`)
type Bound = (Version, bool);

/// Whether the node-semver range `range` admits some release of Node
/// `major`, or `None` when it can't be read (pre-release tags, build
/// metadata, anything that isn't a comparator). The `node:N` tag follows the
/// major's releases, so it stands for the whole `N.x` line.
fn allows(range: &str, major: u64) -> Option<bool> {
    let mut admitted = false;
    for set in range.split("||") {
        admitted |= set_allows(set, major)?;
    }
    Some(admitted)
}

/// One `||`-separated comparator set: every comparator in it must hold
fn set_allows(set: &str, major: u64) -> Option<bool> {
    // The line's own bounds, narrowed by each comparator in turn
    let mut lo: Bound = ([major, 0, 0], false);
    let mut hi: Bound = ([major + 1, 0, 0], false);

    // `>= 18` is as valid as `>=18`: glue a bare operator to its version
    let mut tokens: Vec<String> = Vec::new();
    let mut pending = String::new();
    for token in set.split_whitespace() {
        if token != "-" && token.chars().all(|c| "<>=~^".contains(c)) {
            pending.push_str(token);
        } else {
            tokens.push(std::mem::take(&mut pending) + token);
        }
    }
    if !pending.is_empty() {
        return None;
    }

    let mut constraints = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        // Hyphen range `A - B`: `>=A` and `<=B`
        if tokens.get(i + 1).is_some_and(|t| t == "-") {
            let from = partial(tokens[i].as_str())?;
            let to = partial(tokens.get(i + 2)?)?;
            constraints.extend(desugar(">=", &from)?);
            constraints.extend(desugar("<=", &to)?);
            i += 3;
            continue;
        }
        let token = tokens[i].as_str();
        let op = [">=", "<=", "~>", ">", "<", "~", "^", "="]
            .into_iter()
            .find(|op| token.starts_with(op))
            .unwrap_or("");
        constraints.extend(desugar(op, &partial(&token[op.len()..])?)?);
        i += 1;
    }

    for (op, v) in constraints {
        match op {
            ">=" => lo = lo.max((v, false)),
            ">" => lo = lo.max((v, true)),
            "<=" => hi = hi.min((v, true)),
            "<" => hi = hi.min((v, false)),
            _ => unreachable!("desugar only emits these four"),
        }
    }
    Some(lo.0 < hi.0 || (lo.0 == hi.0 && !lo.1 && hi.1))
}

/// A possibly partial version (`18`, `18.2`, `v18.2.1`, `18.x`), as the
/// components given before any wildcard
fn partial(version: &str) -> Option<Vec<u64>> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let mut parts = Vec::new();
    if version.is_empty() {
        return Some(parts);
    }
    let mut wildcard = false;
    for part in version.split('.') {
        if matches!(part, "x" | "X" | "*") {
            wildcard = true;
        } else if wildcard {
            return None;
        } else {
            parts.push(part.parse().ok()?);
        }
    }
    (parts.len() <= 3 && version.split('.').count() <= 3).then_some(parts)
}

/// node-semver's desugaring of `op` applied to a partial version, as plain
/// `>=`/`>`/`<=`/`<` comparators on full versions
fn desugar(op: &str, p: &[u64]) -> Option<Vec<(&'static str, Version)>> {
    let fill = [
        p.first().copied().unwrap_or(0),
        p.get(1).copied().unwrap_or(0),
        p.get(2).copied().unwrap_or(0),
    ];
    // The first version past everything `p` matches: `18` -> 19.0.0
    let bump = || match p.len() {
        1 => [p[0] + 1, 0, 0],
        2 => [p[0], p[1] + 1, 0],
        _ => [p[0], p[1], p[2] + 1],
    };
    Some(match (op, p.len()) {
        // `*`, `>=*`, `<=*`, `~*`, `^*`: anything
        ("" | "=" | ">=" | "<=" | "~" | "~>" | "^", 0) => vec![],
        // `>*` and `<*` match nothing; leave such a range unread
        (">" | "<", 0) => return None,
        ("" | "=", 3) => vec![(">=", fill), ("<=", fill)],
        ("" | "=", _) => vec![(">=", fill), ("<", bump())],
        (">=", _) => vec![(">=", fill)],
        (">", 3) => vec![(">", fill)],
        (">", _) => vec![(">=", bump())],
        ("<", _) => vec![("<", fill)],
        ("<=", 3) => vec![("<=", fill)],
        ("<=", _) => vec![("<", bump())],
        ("~" | "~>", 1) => vec![(">=", fill), ("<", [p[0] + 1, 0, 0])],
        ("~" | "~>", _) => vec![(">=", fill), ("<", [p[0], p[1] + 1, 0])],
        ("^", _) => {
            // Everything up to the next change of the first non-zero part
            let upper = if p[0] > 0 || p.len() == 1 {
                [p[0] + 1, 0, 0]
            } else if p[1] > 0 || p.len() == 2 {
                [0, p[1] + 1, 0]
            } else {
                [0, 0, p[2] + 1]
            };
            vec![(">=", fill), ("<", upper)]
        }
        _ => return None,
    })
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

/// Where the jobs keep the package manager's download cache, and the env
/// that points it there. It lives inside the checkout because GitLab caches
/// must live inside the project directory.
fn dependency_cache(n: &NodeFacts) -> (Vec<(&'static str, &'static str)>, &'static str) {
    match n.package_manager {
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
    }
}

/// Base job with the per-manager image and the dependency cache
fn base_job(id: &str, name: &str, stage: Stage, timeout: u32, n: &NodeFacts) -> Job {
    let (cache_env, cache_path) = dependency_cache(n);
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

/// `prettier --check .`, minus the dependency cache the install step just
/// filled inside the checkout.
///
/// Prettier skips only `node_modules` and VCS directories on its own, so it
/// would otherwise walk into the cache: pnpm's store keeps a minified
/// `*-index.json` per package and bun's holds every package unpacked, and
/// none of that is formatted to the project's style. On a developer's
/// machine the cache lives under $HOME, so only CI tripped over it.
fn prettier_check(n: &NodeFacts) -> String {
    let (_, cache_path) = dependency_cache(n);
    tool(n, &format!("prettier --check . '!{cache_path}**'"))
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
                Step::run("Check formatting", prettier_check(&n)),
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
                commands
                    .iter()
                    .any(|c| c.starts_with("./node_modules/.bin/prettier --check . ")),
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

    /// `npm-shrinkwrap.json` is a lockfile too, and `npm ci` installs from it.
    /// Taking `npm install` instead would discard the pinned tree and resolve
    /// `package.json`'s ranges against the registry on every run.
    #[test]
    fn test_npm_shrinkwrap_is_a_lockfile_too() {
        let facts = facts_with(&[
            ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
            ("npm-shrinkwrap.json", "{}"),
        ]);
        let jobs = NodeTest::default().jobs(&facts);
        let commands: Vec<&String> = jobs[0]
            .steps
            .iter()
            .filter_map(|s| match s {
                Step::Run { command, .. } => Some(command),
                _ => None,
            })
            .collect();
        assert!(commands.iter().any(|c| *c == "npm ci"), "{commands:?}");
        assert!(
            !commands.iter().any(|c| *c == "npm install"),
            "{commands:?}"
        );
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

    fn image_for(engines: &str) -> &'static str {
        let manifest = serde_json::json!({"engines": {"node": engines}}).to_string();
        default_image(&facts_with(&[
            ("package.json", &manifest),
            ("pnpm-lock.yaml", ""),
        ]))
    }

    /// pnpm and Yarn Classic refuse to install a project whose own
    /// `engines.node` excludes the running Node, so the image has to be one
    /// the project admits
    #[test]
    fn test_image_follows_engines_node() {
        // The default stands whenever it's allowed
        for range in [
            ">=18",
            ">= 18",
            "^22",
            "~22.1",
            "22.x",
            "22",
            "v22.3.0",
            ">=18 <23",
            "18 - 22",
            "^18 || ^20 || ^22 || ^24",
            "*",
            ">=16.14.0 <=22",
        ] {
            assert_eq!(image_for(range), "node:22", "{range}");
        }
        // Pinned to an older line: the newest one it allows
        assert_eq!(image_for("20.x"), "node:20");
        assert_eq!(image_for("^18"), "node:18");
        assert_eq!(image_for(">=18 <21"), "node:20");
        assert_eq!(image_for("<22"), "node:20");
        assert_eq!(image_for("<=20"), "node:20");
        assert_eq!(image_for("16 - 20.5"), "node:20");
        assert_eq!(image_for("~16.20.2"), "node:16");
        assert_eq!(image_for("^16 || ^18"), "node:18");
        // Needs a newer one: the oldest it allows
        assert_eq!(image_for(">=24"), "node:24");
        assert_eq!(image_for(">22"), "node:24");
        // `>22.0.0` still admits later 22.x releases
        assert_eq!(image_for(">22.0.0"), "node:22");
        // Equidistant from the default: the newer one
        assert_eq!(image_for("^20 || ^24"), "node:24");
    }

    /// Anything unreadable or unsatisfiable keeps the default rather than
    /// guessing; the package manager then names the real problem
    #[test]
    fn test_image_falls_back_to_default() {
        for range in [
            ">=22.0.0-rc.1",
            "latest",
            ">=",
            ">=30",
            "<16",
            ">=22 <20",
            "22.0.0 - ",
        ] {
            assert_eq!(image_for(range), "node:22", "{range:?}");
        }
        assert_eq!(default_image(&ProjectFacts::default()), "node:22");
    }

    /// Every node job runs on the admitted image, and bun keeps its own
    #[test]
    fn test_jobs_use_the_admitted_image() {
        let facts = facts_with(&[
            (
                "package.json",
                r#"{"engines": {"node": "20.x"}, "scripts": {"test": "jest"}}"#,
            ),
            ("yarn.lock", "# yarn lockfile v1\n"),
        ]);
        let rules: [&dyn Rule; 4] = [&NodeTest::default(), &NodeLint, &NodeTypecheck, &NodeFmt];
        for rule in rules {
            for job in rule.jobs(&facts) {
                assert_eq!(job.image.as_deref(), Some("node:20"), "{}", job.id);
            }
        }

        let facts = facts_with(&[
            ("package.json", r#"{"engines": {"node": "20.x"}}"#),
            ("bun.lock", ""),
        ]);
        assert_eq!(default_image(&facts), "oven/bun:1");
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
            commands.contains(&"yarn exec prettier --check . '!.yarn-cache/**'".to_string()),
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
            commands
                .contains(&"./node_modules/.bin/prettier --check . '!.yarn-cache/**'".to_string()),
            "{commands:?}"
        );
    }

    #[test]
    fn test_prettier_skips_the_dependency_cache() {
        // Every manager's cache lives inside the checkout so the backends can
        // archive it, and prettier only skips node_modules on its own: pnpm's
        // store and bun's unpacked packages would fail `--check` on files that
        // aren't the project's
        for (lockfile, cache) in [
            ("package-lock.json", ".npm-cache/"),
            ("pnpm-lock.yaml", ".pnpm-store/"),
            ("bun.lockb", ".bun-cache/"),
            ("yarn.lock", ".yarn-cache/"),
        ] {
            let facts = facts_with(&[
                ("package.json", r#"{"devDependencies": {"prettier": "^3"}}"#),
                (".prettierrc", "{}"),
                (lockfile, ""),
            ]);
            let job = &NodeFmt.jobs(&facts)[0];
            assert_eq!(job.cache.as_ref().unwrap().paths, vec![cache.to_string()]);
            let check = job
                .steps
                .iter()
                .find_map(|s| match s {
                    Step::Run { command, .. } if command.contains("prettier") => Some(command),
                    _ => None,
                })
                .unwrap();
            assert!(
                check.ends_with(&format!("prettier --check . '!{cache}**'")),
                "{lockfile}: {check}"
            );
        }
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
