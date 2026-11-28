use crate::config::Platform;
use crate::ir::{Job, RunnerOs, Step};
use crate::platforms::github::models::{
    GitHubDefaults, GitHubJob, GitHubMatrix, GitHubMatrixInclude, GitHubRunDefaults, GitHubStep,
    GitHubStrategy, GitHubTriggerConfig, GitHubWorkflow,
};
use std::collections::BTreeMap;

/// Which workflow file is being produced. CI runs on branch pushes and pull
/// requests; Release runs only on version tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowKind {
    Ci,
    Release,
}

/// What keeps a tags-only job — the ones holding the publish credentials —
/// from running on a branch push: the workflow's own triggers, or an `if:`
/// on each such job.
///
/// Rendering a file from scratch, the two are the same question: the release
/// workflow is written with `on: push: tags: [v*]` and nothing else. A merge
/// into a file that already exists is not, because `update` leaves the user's
/// triggers alone — see [`tag_gate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagGate {
    /// The workflow only runs on `v*` tag pushes, so its jobs need nothing
    Workflow,
    /// The workflow can run otherwise, so every tags-only job carries the gate
    Job,
}

impl TagGate {
    /// The gate implied by a file cibox writes whole
    pub fn of(kind: WorkflowKind) -> Self {
        match kind {
            WorkflowKind::Ci => TagGate::Job,
            WorkflowKind::Release => TagGate::Workflow,
        }
    }
}

/// Whether the triggers `on` restrict a workflow to `v*` tag pushes, which
/// is the only thing standing between a tags-only job and a branch push in a
/// release workflow: the jobs in that file carry no `if:` of their own.
///
/// `update` merges into an existing workflow and keeps whatever triggers it
/// finds there, so the file's `on:` is not cibox's to assume. The shape that
/// gates is the one cibox writes itself:
///
/// ```yaml
/// on:
///   push:
///     tags:
///     - v*
/// ```
///
/// Any other trigger beside it (`pull_request`, `workflow_dispatch`,
/// `schedule`), a `branches` filter sharing the push — GitHub ORs the two, so
/// branch pushes fire it — or a tag pattern that admits more than `v*` all
/// mean the workflow can run on something that is not a version tag, and the
/// gate has to move onto the jobs.
///
/// Narrow on purpose. A wrong [`TagGate::Job`] costs an `if:` that is true on
/// a `v*` tag push anyway; a wrong [`TagGate::Workflow`] takes the gate off a
/// job that publishes releases.
pub fn tag_gate(on: &serde_yaml::Value) -> TagGate {
    // `on: [push]` and `on: push` name triggers without configuring them,
    // so neither can carry a tag filter
    let Some(triggers) = on.as_mapping() else {
        return TagGate::Job;
    };
    let mut triggers = triggers.iter();
    let (Some((event, push)), None) = (triggers.next(), triggers.next()) else {
        return TagGate::Job;
    };
    if event.as_str() != Some("push") {
        return TagGate::Job;
    }
    let Some(push) = push.as_mapping() else {
        return TagGate::Job;
    };
    let mut tags = None;
    for (key, value) in push {
        match key.as_str() {
            Some("tags") => tags = Some(value),
            // `branches`, `paths`, `tags-ignore`, anything unrecognized
            _ => return TagGate::Job,
        }
    }
    let gated = match tags {
        // A lone pattern may be written as a string
        Some(serde_yaml::Value::String(pattern)) => pattern.starts_with('v'),
        Some(serde_yaml::Value::Sequence(patterns)) => {
            !patterns.is_empty()
                && patterns
                    .iter()
                    .all(|p| p.as_str().is_some_and(|p| p.starts_with('v')))
        }
        _ => false,
    };
    if gated {
        TagGate::Workflow
    } else {
        TagGate::Job
    }
}

/// Lower for GitHub Actions or Gitea Actions, which share the workflow
/// format but not the runner (see [`NODE_SETUP`]).
pub fn lower_github(jobs: &[Job], kind: WorkflowKind, platform: Platform) -> GitHubWorkflow {
    lower_github_gated(jobs, kind, TagGate::of(kind), platform)
}

/// Lower into a file whose triggers are already decided, which is what a
/// merge has: `gate` says whether they gate the tags-only jobs.
pub fn lower_github_gated(
    jobs: &[Job],
    kind: WorkflowKind,
    gate: TagGate,
    platform: Platform,
) -> GitHubWorkflow {
    debug_assert!(
        matches!(platform, Platform::GitHub | Platform::Gitea),
        "{platform:?} does not use the GitHub Actions format"
    );
    let lowered = jobs
        .iter()
        .map(|job| (job.id.clone(), lower_job(job, gate, platform)))
        .collect();

    let on = match kind {
        WorkflowKind::Ci => {
            let branches = Some(vec!["main".to_string(), "master".to_string()]);
            BTreeMap::from([
                (
                    "push".to_string(),
                    GitHubTriggerConfig {
                        branches: branches.clone(),
                        tags: None,
                    },
                ),
                (
                    "pull_request".to_string(),
                    GitHubTriggerConfig {
                        branches,
                        tags: None,
                    },
                ),
            ])
        }
        WorkflowKind::Release => BTreeMap::from([(
            "push".to_string(),
            GitHubTriggerConfig {
                branches: None,
                tags: Some(vec!["v*".to_string()]),
            },
        )]),
    };

    GitHubWorkflow {
        name: match kind {
            WorkflowKind::Ci => "CI".to_string(),
            WorkflowKind::Release => "Release".to_string(),
        },
        on,
        // Least privilege: checking out the code is all any job gets unless
        // it asks for more. Without this key the token inherits the
        // repository default, which on older repos and organizations is
        // write-all — handing every `run:` step push access.
        permissions: base_permissions(),
        jobs: lowered,
    }
}

/// The scopes every job needs: read the repository so checkout works
fn base_permissions() -> BTreeMap<String, String> {
    BTreeMap::from([("contents".to_string(), "read".to_string())])
}

/// Marks the checkout as safe for the git inside a job container.
///
/// GitHub creates the workspace on the host as the runner user and mounts it
/// into the container, whose steps run as root. Git refuses to work in a
/// repository owned by someone else ("detected dubious ownership"), and the
/// exception actions/checkout adds for itself lives in a temporary global
/// config that is gone by the next step — so `go build` (which asks git for
/// the VCS stamp of every binary) and `git describe` in the release jobs fail.
/// `$GITHUB_WORKSPACE` is the container-side path; `${{ github.workspace }}`
/// would be the host's. Images without git (oven/bun) have no repository to
/// protect either: checkout fell back to a tarball there.
const SAFE_DIRECTORY: &str = "if command -v git >/dev/null; then git config --global --add safe.directory \"$GITHUB_WORKSPACE\"; fi";

/// Installs the latest Node.js LTS into a Gitea job container that has none.
///
/// actions/checkout, actions/cache and actions/upload-artifact are JavaScript
/// actions. GitHub's runner brings its own node into a job container to run
/// them; Gitea's act_runner execs the container's `node` instead, so a job in
/// a toolchain image (`rust:latest`, `golang:1`, ...) dies at the checkout
/// with `exec: "node": executable file not found in $PATH`.
///
/// The LTS is the newest release `index.tab` gives a codename in its `lts`
/// column. awk reads the whole index rather than exiting at the first match,
/// which would kill curl with SIGPIPE under `pipefail`. Each step is chained
/// with `&&` rather than relying on `-e`, which leaves failures in an `&&`
/// list alone, and the tarball is checked against the release's
/// SHASUMS256.txt. Every image cibox runs here is glibc-based and ships curl.
const NODE_SETUP: &str = "if ! command -v node >/dev/null; then \
    case \"$(uname -m)\" in x86_64) arch=x64 ;; aarch64) arch=arm64 ;; \
    *) echo \"No Node.js build for $(uname -m)\" >&2; exit 1 ;; esac; \
    version=$(curl -fsSL https://nodejs.org/dist/index.tab \
    | awk -F '\\t' 'NR > 1 && $10 != \"-\" && !v { v = $1 } END { print v }'); \
    tarball=\"node-$version-linux-$arch.tar.gz\"; \
    [ -n \"$version\" ] && \
    curl -fsSL \"https://nodejs.org/dist/$version/$tarball\" -o \"/tmp/$tarball\" && \
    (cd /tmp && curl -fsSL \"https://nodejs.org/dist/$version/SHASUMS256.txt\" \
    | grep \" $tarball\\$\" | sha256sum -c -) && \
    mkdir -p /usr/local/lib/nodejs && \
    tar -xzf \"/tmp/$tarball\" -C /usr/local/lib/nodejs --strip-components=1 && \
    ln -sf /usr/local/lib/nodejs/bin/node /usr/local/bin/node && \
    rm \"/tmp/$tarball\"; \
    fi";

/// Moves the scratch space of a Gitea docker job onto podman's storage.
///
/// act_runner runs even a docker job inside a job container, and the `docker`
/// there may be podman. Since podman 6, `podman build` mounts an overlay over
/// the build context with its upper directory under `$TMPDIR` (`/var/tmp` by
/// default), which is on the container's overlayfs root — and the kernel
/// refuses overlayfs as an upperdir, so every build fails with "mounting an
/// overlay over build context directory ... invalid argument".
/// `/var/lib/containers` is podman's root storage, the path a podman-in-a-
/// container setup puts on host storage (the `quay.io/podman` images declare
/// it a volume). It can't sit in the checkout instead: an upperdir inside the
/// lowerdir is rejected as overlapping, and would land in the context too.
/// A job that can't create it (an unprivileged host runner) keeps its TMPDIR.
const BUILD_TMPDIR: &str = "if mkdir -p /var/lib/containers/tmp 2>/dev/null; then \
    echo \"TMPDIR=/var/lib/containers/tmp\" >> \"$GITHUB_ENV\"; \
    fi";

/// Whether every image the job runs in already ships node
fn ships_node(job: &Job) -> bool {
    let is_node = |image: &str| image.starts_with("node:");
    match &job.matrix {
        Some(entries) => entries.iter().all(|e| is_node(&e.image)),
        None => job.image.as_deref().is_some_and(is_node),
    }
}

fn lower_job(job: &Job, gate: TagGate, platform: Platform) -> GitHubJob {
    // Docker-daemon jobs run directly on the host runner
    let container = if job.needs_docker {
        None
    } else if job.matrix.is_some() {
        Some("${{ matrix.image }}".to_string())
    } else {
        job.image.clone()
    };

    let mut steps = Vec::new();

    for step in &job.steps {
        match step {
            Step::Checkout { full_history } => {
                // actions/checkout leaves the job's token in .git/config as an
                // auth header unless told not to, where every later `run:`
                // step — build scripts, test suites, any dependency code they
                // execute — can read it. Nothing cibox generates talks to git
                // after the clone, so the credential is pure exposure: at
                // minimum read access to a private repository, and whatever
                // the job's extra scopes grant (e.g. `packages: write` on a
                // ghcr.io push).
                let mut with = BTreeMap::from([(
                    "persist-credentials".to_string(),
                    serde_yaml::Value::Bool(false),
                )]);
                if *full_history {
                    with.insert(
                        "fetch-depth".to_string(),
                        serde_yaml::Value::Number(0.into()),
                    );
                }
                steps.push(GitHubStep {
                    name: "Checkout code".to_string(),
                    uses: Some("actions/checkout@v4".to_string()),
                    run: None,
                    with: Some(with),
                });
                if container.is_some() {
                    steps.push(GitHubStep {
                        name: "Trust the checkout".to_string(),
                        uses: None,
                        run: Some(SAFE_DIRECTORY.to_string()),
                        with: None,
                    });
                }
                if let Some(cache) = &job.cache {
                    // Matrix legs run different toolchains; sharing one cache
                    // key would make them overwrite each other
                    let base = match &job.matrix {
                        Some(_) => format!("{}-${{{{ matrix.version }}}}", cache.key),
                        None => cache.key.clone(),
                    };
                    // A cache entry is immutable, and a hit on the exact key
                    // skips the save: with a constant key the first run's
                    // contents would be restored forever and nothing built
                    // since would ever be kept. A key unique to the run is
                    // always saved, and `restore-keys` hands the next run the
                    // newest one. The `-run` in the prefix keeps a plain
                    // job's `rust-cache-run` from matching a matrix leg's
                    // `rust-cache-1.85-run…`.
                    let prefix = format!("{base}-run");
                    steps.push(GitHubStep {
                        name: "Cache".to_string(),
                        uses: Some("actions/cache@v4".to_string()),
                        run: None,
                        with: Some(BTreeMap::from([
                            (
                                "path".to_string(),
                                serde_yaml::Value::String(cache.paths.join("\n")),
                            ),
                            (
                                "key".to_string(),
                                serde_yaml::Value::String(format!(
                                    "{prefix}${{{{ github.run_id }}}}"
                                )),
                            ),
                            (
                                "restore-keys".to_string(),
                                serde_yaml::Value::String(prefix),
                            ),
                        ])),
                    });
                }
            }
            Step::Run { name, command } => {
                steps.push(GitHubStep {
                    name: name.clone(),
                    uses: None,
                    run: Some(command.clone()),
                    with: None,
                });
            }
        }
    }

    for path in &job.artifacts {
        steps.push(GitHubStep {
            name: "Upload artifacts".to_string(),
            uses: Some("actions/upload-artifact@v4".to_string()),
            run: None,
            with: Some(BTreeMap::from([
                (
                    "name".to_string(),
                    serde_yaml::Value::String(format!("{}-artifacts", job.id)),
                ),
                ("path".to_string(), serde_yaml::Value::String(path.clone())),
            ])),
        });
    }

    // Before the first action, which on Gitea needs the container's own node
    if platform == Platform::Gitea
        && container.is_some()
        && !ships_node(job)
        && steps.iter().any(|s| s.uses.is_some())
    {
        steps.insert(
            0,
            GitHubStep {
                name: "Set up Node".to_string(),
                uses: None,
                run: Some(NODE_SETUP.to_string()),
                with: None,
            },
        );
    }

    // Through $GITHUB_ENV, so it holds for every docker command that follows
    if platform == Platform::Gitea && job.needs_docker && job.runs_on == RunnerOs::Linux {
        steps.insert(
            0,
            GitHubStep {
                name: "Set up build scratch space".to_string(),
                uses: None,
                run: Some(BUILD_TMPDIR.to_string()),
                with: None,
            },
        );
    }

    let mut env: BTreeMap<String, String> = job.env.iter().cloned().collect();
    // A credential reads from the secrets context and only from there. A
    // repository Variable is not a weaker secret, it is not a secret at all:
    // its value is shown in the settings UI, handed back in plaintext by the
    // variables API, and never registered for log masking — so a token that
    // lives there is printed verbatim the first time anything echoes its
    // environment. Offering `|| vars.NAME` for one would tell users that
    // keeping it there works, which is exactly the problem: it does work.
    for secret in &job.secrets {
        env.insert(secret.clone(), format!("${{{{ secrets.{secret} }}}}"));
    }
    // Configuration is different: nothing is exposed by keeping a registry
    // username in plain Variables, and plenty of users do, so `||` takes
    // whichever context has the value.
    for name in &job.variables {
        env.insert(
            name.clone(),
            format!("${{{{ secrets.{name} || vars.{name} }}}}"),
        );
    }

    // A job-level block replaces the workflow-level one rather than adding to
    // it, so the extra scopes have to be spelled out alongside the base ones
    let permissions = (!job.permissions.is_empty()).then(|| {
        let mut permissions = base_permissions();
        permissions.extend(job.permissions.iter().cloned());
        permissions
    });

    GitHubJob {
        name: job
            .matrix
            .as_ref()
            .map(|_| format!("{} (${{{{ matrix.version }}}})", job.name)),
        runs_on: match job.runs_on {
            RunnerOs::Linux => "ubuntu-latest",
            RunnerOs::Windows => "windows-latest",
        }
        .to_string(),
        strategy: job.matrix.as_ref().map(|entries| GitHubStrategy {
            fail_fast: false,
            matrix: GitHubMatrix {
                include: entries
                    .iter()
                    .map(|e| GitHubMatrixInclude {
                        version: e.version.clone(),
                        image: e.image.clone(),
                    })
                    .collect(),
            },
        }),
        // Generated commands are POSIX shell; windows-latest ships Git Bash
        defaults: (job.runs_on == RunnerOs::Windows).then(|| GitHubDefaults {
            run: GitHubRunDefaults {
                shell: "bash".to_string(),
            },
        }),
        container,
        permissions,
        env: (!env.is_empty()).then_some(env),
        steps,
        needs: (!job.needs.is_empty()).then(|| job.needs.clone()),
        timeout_minutes: job.timeout_minutes,
        // A tags-only job must not run on branch pushes or PRs, so unless
        // the workflow's own triggers already see to that, it says so itself
        if_expr: (job.tags_only && gate == TagGate::Job)
            .then(|| "startsWith(github.ref, 'refs/tags/v')".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, MatrixEntry, Stage, Step};

    #[test]
    fn test_job_keys_are_rule_ids() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        assert!(workflow.jobs.contains_key("rust-test"));
    }

    #[test]
    fn test_checkout_full_history() {
        let workflow = lower_github(
            &[Job::new("gitleaks", "Gitleaks", Stage::Security)
                .with_steps(vec![Step::checkout_full_history()])],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        let step = &workflow.jobs["gitleaks"].steps[0];
        assert_eq!(step.uses.as_deref(), Some("actions/checkout@v4"));
        assert_eq!(
            step.with.as_ref().unwrap()["fetch-depth"],
            serde_yaml::Value::Number(0.into())
        );
    }

    #[test]
    fn test_checkout_does_not_persist_credentials() {
        let workflow = lower_github(
            &[
                Job::new("rust-test", "Cargo test", Stage::Test).with_steps(vec![Step::checkout()]),
                Job::new("gitleaks", "Gitleaks", Stage::Security)
                    .with_steps(vec![Step::checkout_full_history()]),
            ],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        for id in ["rust-test", "gitleaks"] {
            let with = workflow.jobs[id].steps[0].with.as_ref().unwrap();
            assert_eq!(
                with["persist-credentials"],
                serde_yaml::Value::Bool(false),
                "{id} keeps the token in .git/config"
            );
        }
    }

    #[test]
    fn test_container_and_docker() {
        let workflow = lower_github(
            &[
                Job::new("rust-test", "Cargo test", Stage::Test).with_image("rust:1.80"),
                Job::new("docker-build", "Docker build", Stage::Build)
                    .with_image("ignored")
                    .with_docker(),
            ],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        assert_eq!(
            workflow.jobs["rust-test"].container.as_deref(),
            Some("rust:1.80")
        );
        assert_eq!(workflow.jobs["docker-build"].container, None);
    }

    /// The workspace is the runner user's and container steps run as root,
    /// so without the exception git — and `go build`, which asks it for the
    /// VCS stamp — refuses the checkout as "dubious ownership"
    #[test]
    fn test_container_jobs_trust_the_checkout() {
        let entries = vec![MatrixEntry {
            version: "1.24".to_string(),
            image: "golang:1.24".to_string(),
        }];
        let workflow = lower_github(
            &[
                Job::new("go-build", "Go build", Stage::Build)
                    .with_image("golang:1")
                    .with_steps(vec![Step::checkout(), Step::run("Build", "go build ./...")]),
                Job::new("go-test", "Go test", Stage::Test)
                    .with_matrix(entries)
                    .with_steps(vec![Step::checkout(), Step::run("Test", "go test ./...")]),
                Job::new("docker-release", "Docker push", Stage::Deploy)
                    .with_image("ignored")
                    .with_docker()
                    .with_steps(vec![Step::checkout(), Step::run("Push", "docker push x")]),
            ],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        for id in ["go-build", "go-test"] {
            let steps = &workflow.jobs[id].steps;
            assert_eq!(
                steps[0].uses.as_deref(),
                Some("actions/checkout@v4"),
                "{id}"
            );
            // Straight after the checkout, before anything that runs git
            let run = steps[1].run.as_deref().unwrap();
            assert!(
                run.contains("git config --global --add safe.directory \"$GITHUB_WORKSPACE\""),
                "{id}: {run}"
            );
        }
        // On the host the runner user owns its own workspace
        let host_runs: Vec<&str> = workflow.jobs["docker-release"]
            .steps
            .iter()
            .filter_map(|s| s.run.as_deref())
            .collect();
        assert!(
            !host_runs.iter().any(|r| r.contains("safe.directory")),
            "{host_runs:?}"
        );
    }

    fn node_setup_count(workflow: &GitHubWorkflow, id: &str) -> usize {
        workflow.jobs[id]
            .steps
            .iter()
            .filter(|s| s.run.as_deref() == Some(NODE_SETUP))
            .count()
    }

    /// act_runner runs JavaScript actions with the job container's node, so
    /// a Gitea container job without one has to install it before checkout
    #[test]
    fn test_gitea_container_jobs_set_up_node_before_checkout() {
        let matrix = vec![
            MatrixEntry {
                version: "1.85".to_string(),
                image: "rust:1.85".to_string(),
            },
            MatrixEntry {
                version: "stable".to_string(),
                image: "rust:latest".to_string(),
            },
        ];
        let jobs = [
            Job::new("rust-release", "Cargo publish", Stage::Deploy)
                .with_image("rust:latest")
                .with_steps(vec![
                    Step::checkout(),
                    Step::run("Publish", "cargo publish"),
                ]),
            Job::new("rust-test", "Cargo test", Stage::Test)
                .with_matrix(matrix)
                .with_cache("rust-cache", vec!["target/".to_string()])
                .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")]),
        ];
        let gitea = lower_github(&jobs, WorkflowKind::Ci, Platform::Gitea);
        for id in ["rust-release", "rust-test"] {
            let steps = &gitea.jobs[id].steps;
            assert_eq!(steps[0].run.as_deref(), Some(NODE_SETUP), "{id}");
            assert_eq!(
                steps[1].uses.as_deref(),
                Some("actions/checkout@v4"),
                "{id}"
            );
            assert_eq!(node_setup_count(&gitea, id), 1, "{id}");
        }

        // GitHub's runner brings its own node
        let github = lower_github(&jobs, WorkflowKind::Ci, Platform::GitHub);
        for id in ["rust-release", "rust-test"] {
            assert_eq!(node_setup_count(&github, id), 0, "{id}");
        }
    }

    #[test]
    fn test_gitea_jobs_that_have_node_are_left_alone() {
        let node_matrix = vec![
            MatrixEntry {
                version: "20".to_string(),
                image: "node:20".to_string(),
            },
            MatrixEntry {
                version: "22".to_string(),
                image: "node:22".to_string(),
            },
        ];
        let gitea = lower_github(
            &[
                Job::new("node-test", "Node test", Stage::Test)
                    .with_image("node:22")
                    .with_steps(vec![Step::checkout()]),
                Job::new("node-matrix", "Node test", Stage::Test)
                    .with_matrix(node_matrix)
                    .with_steps(vec![Step::checkout()]),
                // The runner's own image provides node on the host
                Job::new("docker-release", "Docker push", Stage::Deploy)
                    .with_docker()
                    .with_steps(vec![Step::checkout()]),
                Job::new("docker-release-windows", "Docker push", Stage::Deploy)
                    .on_windows()
                    .with_steps(vec![Step::checkout()]),
                // No action to run
                Job::new("shell-only", "Shell", Stage::Test)
                    .with_image("rust:latest")
                    .with_steps(vec![Step::run("Hello", "echo hello")]),
            ],
            WorkflowKind::Ci,
            Platform::Gitea,
        );
        for id in [
            "node-test",
            "node-matrix",
            "docker-release",
            "docker-release-windows",
            "shell-only",
        ] {
            assert_eq!(node_setup_count(&gitea, id), 0, "{id}");
        }
    }

    /// One leg without node is enough to need the setup
    #[test]
    fn test_gitea_mixed_matrix_sets_up_node() {
        let gitea = lower_github(
            &[Job::new("mixed", "Mixed", Stage::Test)
                .with_matrix(vec![
                    MatrixEntry {
                        version: "22".to_string(),
                        image: "node:22".to_string(),
                    },
                    MatrixEntry {
                        version: "1".to_string(),
                        image: "golang:1".to_string(),
                    },
                ])
                .with_steps(vec![Step::checkout()])],
            WorkflowKind::Ci,
            Platform::Gitea,
        );
        assert_eq!(node_setup_count(&gitea, "mixed"), 1);
    }

    fn build_tmpdir_count(workflow: &GitHubWorkflow, id: &str) -> usize {
        workflow.jobs[id]
            .steps
            .iter()
            .filter(|s| s.run.as_deref() == Some(BUILD_TMPDIR))
            .count()
    }

    /// act_runner puts docker jobs in a job container too, whose overlayfs
    /// /var/tmp podman can't overlay the build context from
    #[test]
    fn test_gitea_docker_jobs_move_the_scratch_space_first() {
        let jobs = [
            Job::new("docker-build", "Docker build", Stage::Build)
                .with_docker()
                .with_steps(vec![Step::checkout(), Step::run("Build", "docker build .")]),
            Job::new("docker-release-windows", "Docker push", Stage::Deploy)
                .with_docker()
                .on_windows()
                .with_steps(vec![Step::checkout()]),
            Job::new("rust-test", "Cargo test", Stage::Test)
                .with_image("rust:latest")
                .with_steps(vec![Step::checkout()]),
        ];
        let gitea = lower_github(&jobs, WorkflowKind::Ci, Platform::Gitea);
        let steps = &gitea.jobs["docker-build"].steps;
        assert_eq!(steps[0].run.as_deref(), Some(BUILD_TMPDIR));
        assert_eq!(steps[1].uses.as_deref(), Some("actions/checkout@v4"));
        assert_eq!(build_tmpdir_count(&gitea, "docker-build"), 1);
        // Neither a Windows runner nor a job that runs no docker needs it
        assert_eq!(build_tmpdir_count(&gitea, "docker-release-windows"), 0);
        assert_eq!(build_tmpdir_count(&gitea, "rust-test"), 0);

        // GitHub runs docker jobs on the runner host itself
        let github = lower_github(&jobs, WorkflowKind::Ci, Platform::GitHub);
        assert_eq!(build_tmpdir_count(&github, "docker-build"), 0);
    }

    /// Run the scratch-space step against a directory of the test's own, and
    /// against one it can't create
    #[test]
    fn test_build_tmpdir_is_only_set_when_it_exists() {
        use std::fs;
        use std::path::Path;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let env_file = dir.path().join("github_env");
        let run = |root: &Path| {
            fs::write(&env_file, "").unwrap();
            let script = BUILD_TMPDIR.replace("/var/lib/containers", root.to_str().unwrap());
            let output = Command::new("sh")
                .args(["-ec", &script])
                .env("GITHUB_ENV", &env_file)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            fs::read_to_string(&env_file).unwrap()
        };

        let root = dir.path().join("containers");
        assert_eq!(run(&root), format!("TMPDIR={}/tmp\n", root.display()));
        assert!(root.join("tmp").is_dir());

        // A path under a regular file can't be created by anyone
        let blocked = dir.path().join("github_env").join("containers");
        assert_eq!(run(&blocked), "");
    }

    /// Run the setup step with stub `curl`, `uname` and `node`-less PATH, and
    /// check the version it resolves and the tarball it verifies
    #[test]
    fn test_node_setup_picks_the_newest_lts_and_verifies_it() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        fs::create_dir_all(&bin).unwrap();
        let log = dir.path().join("curl.log");
        let index = "version\tdate\tfiles\tnpm\tv8\tuv\tzlib\topenssl\tmodules\tlts\tsecurity\n\
                     v27.0.0\t-\t-\t-\t-\t-\t-\t-\t-\t-\t-\n\
                     v26.13.1\t-\t-\t-\t-\t-\t-\t-\t-\tLyra\t-\n\
                     v24.9.0\t-\t-\t-\t-\t-\t-\t-\t-\tKrypton\t-\n";
        fs::write(dir.path().join("index.tab"), index).unwrap();
        // curl serves index.tab and a SHASUMS256.txt that no tarball matches,
        // so the step has to stop at the checksum, before touching /usr/local
        let curl = format!(
            "#!/bin/sh\necho \"$@\" >> {log}\n\
             case \"$2\" in\n\
             */index.tab) cat {dir}/index.tab ;;\n\
             */SHASUMS256.txt) echo \"{zeros}  node-v26.13.1-linux-x64.tar.gz\" ;;\n\
             *) echo tarball > \"$4\" ;;\n\
             esac\n",
            log = log.display(),
            dir = dir.path().display(),
            zeros = "0".repeat(64),
        );
        let stubs = [
            ("curl", curl.as_str()),
            ("uname", "#!/bin/sh\necho x86_64\n"),
        ];
        for (name, script) in stubs {
            let path = bin.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // The real tools the step needs, without whatever node the host has
        for tool in ["awk", "grep", "sha256sum", "cat", "sh", "bash"] {
            let found = Command::new("sh")
                .args(["-c", &format!("command -v {tool}")])
                .output()
                .unwrap();
            let target = String::from_utf8(found.stdout).unwrap();
            std::os::unix::fs::symlink(target.trim(), bin.join(tool)).unwrap();
        }

        for shell in [&["-e"][..], &["-eo", "pipefail"][..]] {
            fs::write(&log, "").unwrap();
            let output = Command::new("bash")
                .args(shell)
                .args(["-c", NODE_SETUP])
                .env("PATH", &bin)
                .output()
                .unwrap();
            assert!(
                !output.status.success(),
                "{shell:?}: a bad checksum must fail the step"
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert!(stderr.contains("did NOT match"), "{shell:?}: {stderr}");
            let requested = fs::read_to_string(&log).unwrap();
            assert!(
                requested
                    .contains("https://nodejs.org/dist/v26.13.1/node-v26.13.1-linux-x64.tar.gz"),
                "{shell:?}: {requested}"
            );
        }
        let _ = fs::remove_file("/tmp/node-v26.13.1-linux-x64.tar.gz");
    }

    #[test]
    fn test_secrets_become_job_env() {
        let workflow = lower_github(
            &[Job::new("docker-release", "Docker push", Stage::Deploy)
                .with_secrets(vec![
                    "DOCKER_PASSWORD".to_string(),
                    "GITHUB_TOKEN".to_string(),
                ])
                .with_variables(vec!["DOCKER_USERNAME".to_string()])],
            WorkflowKind::Release,
            Platform::GitHub,
        );
        let env = workflow.jobs["docker-release"].env.as_ref().unwrap();
        // A credential comes from the secrets context and nowhere else
        assert_eq!(env["DOCKER_PASSWORD"], "${{ secrets.DOCKER_PASSWORD }}");
        assert_eq!(env["GITHUB_TOKEN"], "${{ secrets.GITHUB_TOKEN }}");
        // Configuration may come from either store
        assert_eq!(
            env["DOCKER_USERNAME"],
            "${{ secrets.DOCKER_USERNAME || vars.DOCKER_USERNAME }}"
        );
    }

    /// Nothing a rule declares as a credential may be given a `vars`
    /// fallback, on any workflow, for any rule: Variables are readable and
    /// unmasked, so the fallback would make storing a token there work.
    #[test]
    fn test_no_credential_reads_from_the_variables_context() {
        let facts = crate::detection::ProjectFacts::default();
        let mut config = crate::config::CiboxConfig::default();
        for r in crate::rules::resolve(&facts, &config) {
            config.set_enabled_override(r.rule.id(), Some(true));
        }
        let jobs: Vec<Job> = crate::rules::resolve(&facts, &config)
            .iter()
            .flat_map(|r| r.rule.jobs(&facts))
            .collect();

        let mut checked = 0;
        for kind in [WorkflowKind::Ci, WorkflowKind::Release] {
            for (id, lowered) in lower_github(&jobs, kind, Platform::GitHub).jobs {
                let job = jobs.iter().find(|j| j.id == id).unwrap();
                let env = lowered.env.unwrap_or_default();
                for secret in &job.secrets {
                    assert_eq!(
                        env[secret],
                        format!("${{{{ secrets.{secret} }}}}"),
                        "{id} reads the credential {secret} from somewhere other than Secrets"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no job declared a credential to check");
    }

    #[test]
    fn test_release_workflow_triggers_on_tags_only() {
        let workflow = lower_github(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Release,
            Platform::GitHub,
        );
        let triggers = &workflow.on;
        assert_eq!(workflow.name, "Release");
        assert_eq!(triggers["push"].tags, Some(vec!["v*".to_string()]));
        assert_eq!(triggers["push"].branches, None);
        assert!(!triggers.contains_key("pull_request"));
        // No per-job guard needed in a tag-gated workflow
        assert_eq!(workflow.jobs["rust-release"].if_expr, None);
    }

    #[test]
    fn test_tags_only_job_in_ci_workflow_gets_if_guard() {
        let workflow = lower_github(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        assert_eq!(
            workflow.jobs["rust-release"].if_expr.as_deref(),
            Some("startsWith(github.ref, 'refs/tags/v')")
        );
    }

    /// The canonical release triggers, as `tag_gate` sees them coming back
    /// off disk
    fn triggers(yaml: &str) -> serde_yaml::Value {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn test_only_a_v_tag_push_gates_the_workflow() {
        // What cibox writes for a release workflow, in both tag spellings
        for gated in [
            "push:\n  tags:\n  - v*\n",
            "push:\n  tags: [v*]\n",
            "push:\n  tags: [v*, v?.?.?]\n",
            "push:\n  tags: v*\n",
        ] {
            assert_eq!(tag_gate(&triggers(gated)), TagGate::Workflow, "{gated}");
        }

        for open in [
            // Any trigger that is not a tag push can fire on a branch
            "pull_request:\n",
            "push: null\n",
            "[push]\n",
            "push\n",
            "workflow_dispatch:\n",
            "push:\n  branches: [main]\n",
            "schedule:\n- cron: 0 0 * * *\n",
            // ...including one sitting next to the tag push
            "push:\n  tags: [v*]\npull_request:\n",
            "workflow_dispatch:\npush:\n  tags: [v*]\n",
            // GitHub ORs the two filters, so branch pushes fire this too
            "push:\n  branches: [main]\n  tags: [v*]\n",
            // Tags that are not version tags
            "push:\n  tags: ['*']\n",
            "push:\n  tags: [v*, nightly]\n",
            "push:\n  tags: []\n",
            "push:\n  tags-ignore: [draft*]\n",
        ] {
            assert_eq!(tag_gate(&triggers(open)), TagGate::Job, "{open}");
        }
    }

    #[test]
    fn test_tags_only_job_gated_by_the_job_when_the_workflow_does_not() {
        // A release workflow whose triggers were changed: the file no longer
        // gates the job, so the job has to
        let workflow = lower_github_gated(
            &[Job::new("rust-release", "Cargo publish", Stage::Deploy).tags_only()],
            WorkflowKind::Release,
            TagGate::Job,
            Platform::GitHub,
        );
        assert_eq!(
            workflow.jobs["rust-release"].if_expr.as_deref(),
            Some("startsWith(github.ref, 'refs/tags/v')")
        );
        // Jobs that run on every push are not gated either way
        let workflow = lower_github_gated(
            &[Job::new("rust-test", "Cargo test", Stage::Test)],
            WorkflowKind::Ci,
            TagGate::Job,
            Platform::GitHub,
        );
        assert_eq!(workflow.jobs["rust-test"].if_expr, None);
    }

    #[test]
    fn test_windows_job_runs_on_windows_with_bash() {
        let workflow = lower_github(
            &[
                Job::new("docker-release-windows", "Docker push (windows)", Stage::Deploy)
                    .on_windows(),
                Job::new("docker-release-linux", "Docker push (linux)", Stage::Deploy),
            ],
            WorkflowKind::Release,
            Platform::GitHub,
        );
        let windows = &workflow.jobs["docker-release-windows"];
        assert_eq!(windows.runs_on, "windows-latest");
        assert_eq!(windows.defaults.as_ref().unwrap().run.shell, "bash");
        let linux = &workflow.jobs["docker-release-linux"];
        assert_eq!(linux.runs_on, "ubuntu-latest");
        assert_eq!(linux.defaults, None);
    }

    #[test]
    fn test_workflow_token_is_read_only_by_default() {
        for kind in [WorkflowKind::Ci, WorkflowKind::Release] {
            let workflow = lower_github(
                &[Job::new("rust-test", "Cargo test", Stage::Test)],
                kind,
                Platform::GitHub,
            );
            assert_eq!(workflow.permissions["contents"], "read", "{kind:?}");
            // Nothing declared, so the job inherits the workflow scopes
            assert_eq!(workflow.jobs["rust-test"].permissions, None, "{kind:?}");
        }
    }

    #[test]
    fn test_declared_permissions_are_added_to_the_base_scopes() {
        let workflow = lower_github(
            &[Job::new("docker-release", "Docker push", Stage::Deploy)
                .with_permission("packages", "write")],
            WorkflowKind::Release,
            Platform::GitHub,
        );
        let permissions = workflow.jobs["docker-release"]
            .permissions
            .as_ref()
            .unwrap();
        assert_eq!(permissions["packages"], "write");
        // A job-level block overrides the workflow one outright, so checkout
        // would break without the base scope repeated here
        assert_eq!(permissions["contents"], "read");
    }

    #[test]
    fn test_matrix_job() {
        let entries = vec![
            MatrixEntry {
                version: "1.85".to_string(),
                image: "rust:1.85".to_string(),
            },
            MatrixEntry {
                version: "nightly".to_string(),
                image: "rustlang/rust:nightly".to_string(),
            },
        ];
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])
                .with_matrix(entries)],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        let job = &workflow.jobs["rust-test"];
        assert_eq!(job.name.as_deref(), Some("Cargo test (${{ matrix.version }})"));
        assert_eq!(job.container.as_deref(), Some("${{ matrix.image }}"));
        let strategy = job.strategy.as_ref().unwrap();
        assert!(!strategy.fail_fast);
        assert_eq!(strategy.matrix.include.len(), 2);
        assert_eq!(strategy.matrix.include[1].image, "rustlang/rust:nightly");
        // Each leg caches under its own key
        let cache = job
            .steps
            .iter()
            .find(|s| s.uses.as_deref() == Some("actions/cache@v4"))
            .unwrap();
        let with = cache.with.as_ref().unwrap();
        assert_eq!(
            with["key"],
            serde_yaml::Value::String(
                "rust-cache-${{ matrix.version }}-run${{ github.run_id }}".to_string()
            )
        );
        assert_eq!(
            with["restore-keys"],
            serde_yaml::Value::String("rust-cache-${{ matrix.version }}-run".to_string())
        );
    }

    #[test]
    fn test_non_matrix_job_has_no_strategy_or_name() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test).with_image("rust:latest")],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        let yaml = serde_yaml::to_string(&workflow).unwrap();
        assert!(!yaml.contains("strategy"), "{yaml}");
        assert!(!yaml.contains("fail-fast"), "{yaml}");
        assert_eq!(workflow.jobs["rust-test"].name, None);
    }

    #[test]
    fn test_cache_lowered_after_checkout() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        let steps = &workflow.jobs["rust-test"].steps;
        assert_eq!(steps[1].uses.as_deref(), Some("actions/cache@v4"));
    }

    /// actions/cache never overwrites an entry and skips the save on an
    /// exact-key hit, so a constant key would freeze the cache at its first
    /// run. Every run has to save under its own key and restore by prefix.
    #[test]
    fn test_cache_saved_per_run_and_restored_by_prefix() {
        let workflow = lower_github(
            &[Job::new("rust-test", "Cargo test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Run tests", "cargo test")])
                .with_cache("rust-cache", vec!["target/".to_string()])],
            WorkflowKind::Ci,
            Platform::GitHub,
        );
        let with = workflow.jobs["rust-test"].steps[1].with.as_ref().unwrap();
        let key = with["key"].as_str().unwrap();
        let restore = with["restore-keys"].as_str().unwrap();
        assert_eq!(key, "rust-cache-run${{ github.run_id }}");
        assert_eq!(restore, "rust-cache-run");
        assert!(key.starts_with(restore));
    }
}
