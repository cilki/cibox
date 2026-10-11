use super::Rule;
use crate::config::image::registry_host;
use crate::config::DockerPlatform;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

fn dockerfile_flag(facts: &ProjectFacts) -> String {
    match &facts.docker {
        Some(d) if d.dockerfile != "Dockerfile" => format!(" -f {}", d.dockerfile),
        _ => String::new(),
    }
}

/// Build the Dockerfile on every push
pub struct DockerBuild {
    /// Image name, e.g. "fossable/cibox"
    pub image: String,
}

impl Rule for DockerBuild {
    fn id(&self) -> &'static str {
        "docker-build"
    }

    fn name(&self) -> &'static str {
        "Docker build"
    }

    fn description(&self) -> &'static str {
        "Build the Dockerfile on every push"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.docker.is_some()
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Build)
            .with_docker()
            .with_timeout(30)
            .with_steps(vec![
                Step::checkout(),
                Step::run(
                    "Build image",
                    format!("docker build{} -t {} .", dockerfile_flag(facts), self.image),
                ),
            ])]
    }
}

/// Reads the pushed git tag into `$version` for the release tags that follow.
/// Shell variables don't survive across steps on GitHub, so every step that
/// pushes a versioned tag starts with this.
const READ_VERSION: &str = "version=\"$(git describe --tags --exact-match)\"; ";

/// Push the image to a registry when a version tag is pushed. The image is
/// pushed under two tags: `latest` and the name of the git tag.
/// The registry is inferred from the image name: a `ghcr.io/` prefix logs in
/// to the GitHub Container Registry with GITHUB_TOKEN, any other host prefix
/// logs in to that registry with Docker Hub-style credentials, and a bare
/// name uses Docker Hub itself. Login is skipped at runtime when the
/// credentials aren't configured.
///
/// With `platforms` set, linux targets build in a single buildx+QEMU job.
/// `WindowsAmd64` cannot be emulated, so it gets its own Windows-runner job
/// (the same Dockerfile must support a Windows base image); when linux and
/// windows are mixed, both jobs push staging tags and a final job merges them
/// into one multi-platform manifest. Staging tags are appended to `image`, so
/// the image name must not already carry a tag.
pub struct DockerRelease {
    pub image: String,
    /// Target platforms; empty = single-arch build/push on the host runner
    pub platforms: Vec<DockerPlatform>,
    /// Push README.md as the Docker Hub repository description after the
    /// release; no effect for ghcr.io images or Windows-only releases, and
    /// skipped at runtime when the credentials or the README are missing
    pub sync_readme: bool,
}

impl DockerRelease {
    fn uses_ghcr(&self) -> bool {
        self.image.starts_with("ghcr.io/")
    }

    /// `-t` flags for the final image: `latest` plus the git tag name, which
    /// a preceding [`READ_VERSION`] leaves in `$version`. The expansion is
    /// quoted, so a hostile tag name stays a single argument for docker to
    /// reject rather than becoming shell.
    fn release_tag_flags(&self) -> String {
        format!(
            "-t {image}:latest -t \"{image}:$version\"",
            image = self.image
        )
    }

    /// The login step skips at runtime when the credentials aren't
    /// configured — a registry may not require any (e.g. a local private
    /// one). GitHub interpolates missing secrets as empty strings while the
    /// other platforms leave the variables unset, so test with `${VAR:-}`.
    fn login_command(&self) -> String {
        if self.uses_ghcr() {
            "if [ -n \"${GITHUB_TOKEN:-}\" ]; then echo \"$GITHUB_TOKEN\" | docker login ghcr.io -u \"$GITHUB_ACTOR\" --password-stdin; else echo \"GITHUB_TOKEN not set; skipping docker login\"; fi"
                .to_string()
        } else {
            // Non-default registries need their host on the login command,
            // or the credentials would go to Docker Hub
            let host = match registry_host(&self.image) {
                Some(host) => format!("{host} "),
                None => String::new(),
            };
            format!(
                "if [ -n \"${{DOCKER_USERNAME:-}}\" ]; then echo \"$DOCKER_PASSWORD\" | docker login {host}-u \"$DOCKER_USERNAME\" --password-stdin; else echo \"DOCKER_USERNAME not set; skipping docker login\"; fi"
            )
        }
    }

    /// The credentials the push authenticates with
    fn secrets(&self) -> Vec<String> {
        if self.uses_ghcr() {
            vec!["GITHUB_TOKEN".to_string()]
        } else {
            vec!["DOCKER_PASSWORD".to_string()]
        }
    }

    /// The registry username, which is configuration and not a credential:
    /// it identifies an account rather than proving ownership of one, and is
    /// public on Docker Hub anyway (it is in the image name). Declaring it
    /// separately is what lets users keep it in plain repository Variables
    /// without the password being invited to follow it there.
    fn variables(&self) -> Vec<String> {
        if self.uses_ghcr() {
            // GITHUB_ACTOR is runner-provided; nothing to declare
            Vec::new()
        } else {
            vec!["DOCKER_USERNAME".to_string()]
        }
    }

    /// README-sync step, when enabled. None for ghcr.io: there is no
    /// description API to target, so the knob is silently ignored.
    ///
    /// The sync reuses the registry credentials, so it is guarded the same way
    /// the login is — plus a check that there is a README to send, since the
    /// bind mount would otherwise have docker create an empty `README.md`
    /// *directory* in the checkout.
    ///
    /// The credentials reach the container through the environment rather than
    /// `-e NAME=VALUE`: the latter writes the registry password into the
    /// `docker` process's argv, where anything able to read `/proc` on the
    /// runner — a concurrent job on a shared or self-hosted runner, another
    /// container sharing the host PID namespace — can read it, and where
    /// `docker inspect` keeps it for the container's lifetime. The prefix
    /// assignment puts it in docker's environment instead and `-e NAME`
    /// forwards it by name, so the value never becomes a command-line
    /// argument. Only README.md is mounted, read-only, instead of handing the
    /// whole checkout to a third-party image with write access.
    fn readme_sync_step(&self) -> Option<Step> {
        (self.sync_readme && !self.uses_ghcr()).then(|| {
            Step::run(
                "Sync README to Docker Hub",
                format!(
                    "if [ -n \"${{DOCKER_USERNAME:-}}\" ] && [ -f README.md ]; then \
                     DOCKER_USER=\"$DOCKER_USERNAME\" DOCKER_PASS=\"$DOCKER_PASSWORD\" \
                     docker run --rm -e DOCKER_USER -e DOCKER_PASS \
                     -v \"$PWD/README.md\":/workspace/README.md:ro \
                     chko/docker-pushrm:1 --file /workspace/README.md {}; \
                     else echo \"DOCKER_USERNAME not set or no README.md; skipping README sync\"; fi",
                    self.image
                ),
            )
        })
    }

    /// Selected platforms deduped and in `DockerPlatform::ALL` order
    fn normalized_platforms(&self) -> Vec<DockerPlatform> {
        DockerPlatform::ALL
            .iter()
            .copied()
            .filter(|p| self.platforms.contains(p))
            .collect()
    }

    /// Steps for a buildx job that builds `linux` platforms and pushes a
    /// single staging `tag`, or the final release tags when `tag` is `None`
    fn buildx_steps(
        &self,
        facts: &ProjectFacts,
        linux: &[DockerPlatform],
        tag: Option<&str>,
    ) -> Vec<Step> {
        let mut steps = vec![
            Step::checkout(),
            Step::run("Login to registry", self.login_command()),
        ];
        // QEMU is only needed to emulate foreign architectures
        if linux.iter().any(|p| *p != DockerPlatform::LinuxAmd64) {
            steps.push(Step::run(
                "Set up QEMU",
                "docker run --privileged --rm tonistiigi/binfmt --install all",
            ));
        }
        steps.push(Step::run("Set up buildx", "docker buildx create --use"));
        let platform_list = linux
            .iter()
            .map(|p| p.as_str())
            .collect::<Vec<_>>()
            .join(",");
        let (read_version, tag_flags) = match tag {
            Some(tag) => ("", format!("-t {tag}")),
            None => (READ_VERSION, self.release_tag_flags()),
        };
        steps.push(Step::run(
            "Build and push image",
            format!(
                "{read_version}docker buildx build --platform {platform_list}{} {tag_flags} --push .",
                dockerfile_flag(facts)
            ),
        ));
        steps
    }

    /// Steps for a plain single-architecture build and push of a staging
    /// `tag` (the Windows-runner job — Windows images can't be cross-built
    /// with buildx), or of the final release tags when `tag` is `None`
    fn build_push_steps(&self, facts: &ProjectFacts, tag: Option<&str>) -> Vec<Step> {
        let build_tag = match tag {
            Some(tag) => tag.to_string(),
            None => format!("{}:latest", self.image),
        };
        let push = match tag {
            Some(tag) => format!("docker push {tag}"),
            None => format!(
                "{READ_VERSION}docker tag {image}:latest \"{image}:$version\"; \
                 docker push {image}:latest; docker push \"{image}:$version\"",
                image = self.image
            ),
        };
        vec![
            Step::checkout(),
            Step::run("Login to registry", self.login_command()),
            Step::run(
                "Build image",
                format!("docker build{} -t {build_tag} .", dockerfile_flag(facts)),
            ),
            Step::run("Push image", push),
        ]
    }

    fn release_job(&self, suffix: &str, name: &str) -> Job {
        let id = format!("{}{suffix}", self.id());
        let job = Job::new(id, name, Stage::Deploy)
            .with_docker()
            .tags_only()
            .with_secrets(self.secrets())
            .with_variables(self.variables());
        if self.uses_ghcr() {
            // ghcr.io authenticates with the ambient CI token, which can't
            // push packages unless the job asks for the scope
            job.with_permission("packages", "write")
        } else {
            job
        }
    }
}

impl Rule for DockerRelease {
    fn id(&self) -> &'static str {
        "docker-release"
    }

    fn owns_job_id(&self, id: &str) -> bool {
        // Per-OS variant jobs emitted when `platforms` mixes Linux and Windows
        id == self.id() || id == "docker-release-linux" || id == "docker-release-windows"
    }

    fn name(&self) -> &'static str {
        "Docker push"
    }

    fn description(&self) -> &'static str {
        "Build and push the image to a registry on version tags"
    }

    fn detect(&self, facts: &ProjectFacts) -> bool {
        facts.docker.is_some()
    }

    fn jobs(&self, facts: &ProjectFacts) -> Vec<Job> {
        let platforms = self.normalized_platforms();
        let (windows, linux): (Vec<DockerPlatform>, Vec<DockerPlatform>) =
            platforms.into_iter().partition(|p| p.is_windows());

        match (linux.is_empty(), windows.is_empty()) {
            // No linux targets: one plain build/push of the final tag, on a
            // Windows runner if that's what was asked for
            (true, windows_empty) => {
                let job = self.release_job("", self.name()).with_timeout(30);
                let job = if windows_empty { job } else { job.on_windows() };
                let mut steps = self.build_push_steps(facts, None);
                // The sync tool is a Linux container, so skip it on Windows
                if windows_empty {
                    steps.extend(self.readme_sync_step());
                }
                vec![job.with_steps(steps)]
            }
            // Linux only: one buildx job pushes the multi-arch manifest
            (false, true) => {
                let mut steps = self.buildx_steps(facts, &linux, None);
                steps.extend(self.readme_sync_step());
                vec![self
                    .release_job("", self.name())
                    .with_timeout(60)
                    .with_steps(steps)]
            }
            // Mixed: stage per-OS images, then merge into one manifest
            (false, false) => {
                let linux_tag = format!("{}:linux", self.image);
                let windows_tag = format!("{}:windows-amd64", self.image);
                vec![
                    self.release_job("-linux", "Docker push (linux)")
                        .with_timeout(60)
                        .with_steps(self.buildx_steps(facts, &linux, Some(&linux_tag))),
                    self.release_job("-windows", "Docker push (windows)")
                        .on_windows()
                        .with_timeout(30)
                        .with_steps(self.build_push_steps(facts, Some(&windows_tag))),
                    self.release_job("", self.name())
                        .with_timeout(10)
                        .with_needs(vec![
                            format!("{}-linux", self.id()),
                            format!("{}-windows", self.id()),
                        ])
                        .with_steps({
                            // imagetools needs no source checkout, but
                            // `git describe` (and the README sync) read
                            // from the repository
                            let mut steps = vec![
                                Step::checkout(),
                                Step::run("Login to registry", self.login_command()),
                                Step::run(
                                    "Merge manifests",
                                    format!(
                                        "{READ_VERSION}docker buildx imagetools create {} {linux_tag} {windows_tag}",
                                        self.release_tag_flags()
                                    ),
                                ),
                            ];
                            steps.extend(self.readme_sync_step());
                            steps
                        }),
                ]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn docker_facts() -> ProjectFacts {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Dockerfile"), "FROM alpine\n").unwrap();
        crate::detection::gather_facts(dir.path())
    }

    #[test]
    fn test_detect_requires_dockerfile() {
        let rule = DockerBuild {
            image: "a/b".to_string(),
        };
        assert!(rule.detect(&docker_facts()));
        assert!(!rule.detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_build_does_not_push() {
        let rule = DockerBuild {
            image: "owner/app".to_string(),
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(job.needs_docker);
        assert!(!job.tags_only);
        assert!(job.secrets.is_empty());
        assert!(!job.steps.iter().any(
            |s| matches!(s, Step::Run { command, .. } if command.starts_with("docker push"))
        ));
    }

    #[test]
    fn test_release_pushes_on_tags_with_dockerhub() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: false,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(job.tags_only);
        // The password is a credential; the username is configuration
        assert_eq!(job.secrets, vec!["DOCKER_PASSWORD"]);
        assert_eq!(job.variables, vec!["DOCKER_USERNAME"]);
        assert_eq!(
            run_command(job, "Build image"),
            "docker build -t owner/app:latest ."
        );
        // The image goes out as both `latest` and the git tag name
        assert_eq!(
            run_command(job, "Push image"),
            "version=\"$(git describe --tags --exact-match)\"; \
             docker tag owner/app:latest \"owner/app:$version\"; \
             docker push owner/app:latest; docker push \"owner/app:$version\""
        );
    }

    fn run_command<'a>(job: &'a Job, name: &str) -> &'a str {
        job.steps
            .iter()
            .find_map(|s| match s {
                Step::Run { name: n, command } if n == name => Some(command.as_str()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("job {} has no step '{name}'", job.id))
    }

    #[test]
    fn test_linux_multiarch_uses_one_buildx_job() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            // Deliberately unsorted; the emitted list is in ALL order
            platforms: vec![DockerPlatform::LinuxArm64, DockerPlatform::LinuxAmd64],
            sync_readme: false,
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "docker-release");
        assert_eq!(job.runs_on, crate::ir::RunnerOs::Linux);
        assert!(job.tags_only);
        assert_eq!(
            run_command(job, "Build and push image"),
            "version=\"$(git describe --tags --exact-match)\"; \
             docker buildx build --platform linux/amd64,linux/arm64 \
             -t owner/app:latest -t \"owner/app:$version\" --push ."
        );
        assert_eq!(
            run_command(job, "Set up QEMU"),
            "docker run --privileged --rm tonistiigi/binfmt --install all"
        );
        assert_eq!(run_command(job, "Set up buildx"), "docker buildx create --use");
    }

    #[test]
    fn test_amd64_only_skips_qemu() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::LinuxAmd64],
            sync_readme: false,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(!job
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { name, .. } if name == "Set up QEMU")));
        assert_eq!(
            run_command(job, "Build and push image"),
            "version=\"$(git describe --tags --exact-match)\"; \
             docker buildx build --platform linux/amd64 \
             -t owner/app:latest -t \"owner/app:$version\" --push ."
        );
    }

    #[test]
    fn test_mixed_platforms_stage_and_merge() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
            sync_readme: false,
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 3);

        let linux = &jobs[0];
        assert_eq!(linux.id, "docker-release-linux");
        assert_eq!(linux.runs_on, crate::ir::RunnerOs::Linux);
        assert_eq!(
            run_command(linux, "Build and push image"),
            "docker buildx build --platform linux/arm64 -t owner/app:linux --push ."
        );

        let windows = &jobs[1];
        assert_eq!(windows.id, "docker-release-windows");
        assert_eq!(windows.runs_on, crate::ir::RunnerOs::Windows);
        assert_eq!(
            run_command(windows, "Build image"),
            "docker build -t owner/app:windows-amd64 ."
        );
        assert_eq!(
            run_command(windows, "Push image"),
            "docker push owner/app:windows-amd64"
        );

        let merge = &jobs[2];
        assert_eq!(merge.id, "docker-release");
        assert_eq!(
            merge.needs,
            vec!["docker-release-linux", "docker-release-windows"]
        );
        assert_eq!(
            run_command(merge, "Merge manifests"),
            "version=\"$(git describe --tags --exact-match)\"; \
             docker buildx imagetools create -t owner/app:latest -t \"owner/app:$version\" \
             owner/app:linux owner/app:windows-amd64"
        );
        // imagetools needs no source checkout, but `git describe` does
        assert!(matches!(merge.steps.first(), Some(Step::Checkout { .. })));

        // Every job logs in, so every job needs the registry credentials
        for job in &jobs {
            assert!(job.tags_only, "{}", job.id);
            assert_eq!(job.secrets, vec!["DOCKER_PASSWORD"], "{}", job.id);
            assert_eq!(job.variables, vec!["DOCKER_USERNAME"], "{}", job.id);
        }
    }

    #[test]
    fn test_windows_only_is_single_windows_job() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64],
            sync_readme: false,
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "docker-release");
        assert_eq!(job.runs_on, crate::ir::RunnerOs::Windows);
        // Windows runners still get a POSIX shell (Git Bash on GitHub), so
        // the version read works there too
        assert_eq!(
            run_command(job, "Push image"),
            "version=\"$(git describe --tags --exact-match)\"; \
             docker tag owner/app:latest \"owner/app:$version\"; \
             docker push owner/app:latest; docker push \"owner/app:$version\""
        );
    }

    #[test]
    fn test_ghcr_image_uses_github_token() {
        let rule = DockerRelease {
            image: "ghcr.io/owner/app".to_string(),
            platforms: vec![],
            sync_readme: false,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert_eq!(job.secrets, vec!["GITHUB_TOKEN"]);
        // GITHUB_ACTOR comes from the runner, so there is nothing to declare
        assert!(job.variables.is_empty());
        let login = run_command(job, "Login to registry");
        assert!(login.contains("docker login ghcr.io"));
        assert!(login.starts_with("if [ -n \"${GITHUB_TOKEN:-}\" ]; then"));
        // The default token can't push packages without this scope
        assert_eq!(
            job.permissions,
            vec![("packages".to_string(), "write".to_string())]
        );
    }

    #[test]
    fn test_login_is_skipped_without_credentials() {
        // A registry may not require credentials at all (e.g. a local
        // private one), so the login step guards on the variables
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: false,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert_eq!(
            run_command(job, "Login to registry"),
            "if [ -n \"${DOCKER_USERNAME:-}\" ]; then echo \"$DOCKER_PASSWORD\" | docker login -u \"$DOCKER_USERNAME\" --password-stdin; else echo \"DOCKER_USERNAME not set; skipping docker login\"; fi"
        );
    }

    #[test]
    fn test_private_registry_login_targets_its_host() {
        let rule = DockerRelease {
            image: "localhost:5000/app".to_string(),
            platforms: vec![],
            sync_readme: false,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(run_command(job, "Login to registry")
            .contains("docker login localhost:5000 -u \"$DOCKER_USERNAME\""));
    }

    #[test]
    fn test_ghcr_permission_covers_every_pushing_job() {
        let rule = DockerRelease {
            image: "ghcr.io/owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
            sync_readme: false,
        };
        for job in rule.jobs(&docker_facts()) {
            assert_eq!(
                job.permissions,
                vec![("packages".to_string(), "write".to_string())],
                "{}",
                job.id
            );
        }
    }

    #[test]
    fn test_non_ghcr_release_needs_no_extra_permissions() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: false,
        };
        assert!(rule.jobs(&docker_facts())[0].permissions.is_empty());
        // Neither does a build that never pushes
        let build = DockerBuild {
            image: "ghcr.io/owner/app".to_string(),
        };
        assert!(build.jobs(&docker_facts())[0].permissions.is_empty());
    }

    const SYNC_STEP: &str = "Sync README to Docker Hub";

    #[test]
    fn test_sync_readme_appends_pushrm_step() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: true,
        };
        let job = &rule.jobs(&docker_facts())[0];
        assert!(matches!(job.steps.last(), Some(Step::Run { name, .. }) if name == SYNC_STEP));
        assert_eq!(
            run_command(job, SYNC_STEP),
            "if [ -n \"${DOCKER_USERNAME:-}\" ] && [ -f README.md ]; then \
             DOCKER_USER=\"$DOCKER_USERNAME\" DOCKER_PASS=\"$DOCKER_PASSWORD\" \
             docker run --rm -e DOCKER_USER -e DOCKER_PASS \
             -v \"$PWD/README.md\":/workspace/README.md:ro \
             chko/docker-pushrm:1 --file /workspace/README.md owner/app; \
             else echo \"DOCKER_USERNAME not set or no README.md; skipping README sync\"; fi"
        );
        // The sync reuses the push credentials, so nothing extra is declared
        assert_eq!(job.secrets, vec!["DOCKER_PASSWORD"]);
        assert_eq!(job.variables, vec!["DOCKER_USERNAME"]);
    }

    #[test]
    fn test_sync_readme_keeps_the_password_out_of_argv() {
        // `-e NAME=VALUE` would expand the registry password into the docker
        // process's command line, readable through /proc on a shared or
        // self-hosted runner and retained by `docker inspect`. The value has
        // to travel in docker's own environment, forwarded by name.
        for platforms in [
            vec![],
            vec![DockerPlatform::LinuxAmd64, DockerPlatform::LinuxArm64],
            vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
        ] {
            let rule = DockerRelease {
                image: "owner/app".to_string(),
                platforms,
                sync_readme: true,
            };
            for job in rule.jobs(&docker_facts()) {
                for step in &job.steps {
                    let Step::Run { command, .. } = step else {
                        continue;
                    };
                    assert!(
                        !command.contains("-e DOCKER_PASS=")
                            && !command.contains("-e DOCKER_USER="),
                        "{}: {command}",
                        job.id
                    );
                }
            }
        }
    }

    #[test]
    fn test_sync_readme_is_skipped_without_credentials() {
        // The login step right before it already skips when the registry
        // secrets aren't configured; an unguarded sync would fail the whole
        // release job instead (GitHub expands a missing secret to "")
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: true,
        };
        let job = &rule.jobs(&docker_facts())[0];
        let sync = run_command(job, SYNC_STEP);
        assert!(
            sync.starts_with("if [ -n \"${DOCKER_USERNAME:-}\" ]"),
            "{sync}"
        );
        // A missing README.md would otherwise have docker create a directory
        // of that name in the checkout
        assert!(sync.contains("[ -f README.md ]"), "{sync}");
    }

    #[test]
    fn test_sync_readme_mounts_only_the_readme_read_only() {
        // A third-party image handed the registry password has no business
        // with a writable mount of the whole checkout
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![],
            sync_readme: true,
        };
        let job = &rule.jobs(&docker_facts())[0];
        let sync = run_command(job, SYNC_STEP);
        assert!(
            sync.contains("-v \"$PWD/README.md\":/workspace/README.md:ro"),
            "{sync}"
        );
        assert!(!sync.contains("-v \"$PWD\":"), "{sync}");
    }

    #[test]
    fn test_sync_readme_in_buildx_job() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::LinuxAmd64, DockerPlatform::LinuxArm64],
            sync_readme: true,
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        assert!(matches!(jobs[0].steps.last(), Some(Step::Run { name, .. }) if name == SYNC_STEP));
    }

    #[test]
    fn test_sync_readme_merge_job_gets_checkout() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
            sync_readme: true,
        };
        let jobs = rule.jobs(&docker_facts());
        // Staging jobs push intermediate tags; only the merge job syncs
        for staging in &jobs[..2] {
            assert!(
                !staging.steps.iter().any(|s| matches!(s, Step::Run { name, .. } if name == SYNC_STEP)),
                "{}",
                staging.id
            );
        }
        let merge = &jobs[2];
        assert!(matches!(merge.steps.first(), Some(Step::Checkout { .. })));
        assert!(matches!(merge.steps.last(), Some(Step::Run { name, .. }) if name == SYNC_STEP));
    }

    #[test]
    fn test_sync_readme_ignored_for_ghcr() {
        let rule = DockerRelease {
            image: "ghcr.io/owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64, DockerPlatform::LinuxArm64],
            sync_readme: true,
        };
        let jobs = rule.jobs(&docker_facts());
        for job in &jobs {
            assert!(
                !job.steps.iter().any(|s| matches!(s, Step::Run { name, .. } if name == SYNC_STEP)),
                "{}",
                job.id
            );
        }
    }

    #[test]
    fn test_sync_readme_ignored_on_windows_runner() {
        let rule = DockerRelease {
            image: "owner/app".to_string(),
            platforms: vec![DockerPlatform::WindowsAmd64],
            sync_readme: true,
        };
        let jobs = rule.jobs(&docker_facts());
        assert_eq!(jobs.len(), 1);
        assert!(!jobs[0]
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { name, .. } if name == SYNC_STEP)));
    }
}
