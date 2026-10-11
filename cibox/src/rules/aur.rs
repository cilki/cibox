use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// makepkg only exists on Arch, so this is the one rule that cannot run on
/// CIBOX_IMAGE; base-devel brings gcc/make/fakeroot/sudo but not git,
/// openssh, pacman-contrib, or namcap — those are installed in the first step.
pub(crate) const IMAGE: &str = "archlinux:base-devel";

/// Where the builder user keeps the key and known_hosts for aur.archlinux.org
const SSH_DIR: &str = "/home/builder/.ssh";

/// Publish packages to the AUR when a version tag is pushed. The AUR repo is
/// the source of truth for each PKGBUILD: the job clones it over SSH, sets
/// pkgver from the tag, refreshes the checksums, test-builds the package with
/// makepkg and lints it with namcap, regenerates .SRCINFO, and pushes.
///
/// Container jobs run as root on every platform while makepkg refuses to run
/// as root, so the steps create a `builder` user and run everything that
/// touches the AUR clone through `runuser`. `builder` gets passwordless
/// pacman so `makepkg -s` can install the PKGBUILD's dependencies — root
/// equivalent, which is acceptable in an ephemeral CI container. `runuser`
/// preserves the caller's environment (GitHub sets HOME=/github/home in
/// container jobs), so every invocation pins HOME to builder's own.
pub struct AurRelease {
    /// AUR package names, one release job each. Resolved by
    /// [`super::aur_packages`] (knob override, else derived from the repo
    /// slug or directory name) and always coerced to valid names before they
    /// reach this struct — they are interpolated into shell commands.
    pub packages: Vec<String>,
}

impl AurRelease {
    fn release_job(&self, suffix: &str, name: impl Into<String>, pkg: &str) -> Job {
        Job::new(format!("{}{suffix}", self.id()), name, Stage::Deploy)
            .with_image(IMAGE)
            .with_timeout(60)
            .tags_only()
            .with_secrets(vec!["AUR_SSH_PRIVATE_KEY".to_string()])
            .with_steps(self.steps(pkg))
    }

    fn steps(&self, pkg: &str) -> Vec<Step> {
        let clone = format!("/home/builder/{pkg}");
        vec![
            // Must precede the checkout: base-devel has neither git nor
            // openssh, and without git in the container actions/checkout
            // falls back to an API tarball with no .git, which would break
            // `git describe` below. `-Syu` rather than `-Sy`: installs
            // against a stale Arch package database routinely 404.
            Step::run(
                "Install build tools",
                "pacman -Syu --noconfirm --needed git openssh pacman-contrib namcap",
            ),
            Step::checkout(),
            Step::run(
                "Create build user",
                "useradd -m builder && echo 'builder ALL=(ALL) NOPASSWD: /usr/bin/pacman' > /etc/sudoers.d/builder",
            ),
            // A missing key fails loudly rather than skipping: the rule only
            // exists when explicitly enabled and the job only runs on tags,
            // so a skip would silently drop releases. GitHub interpolates a
            // missing secret as an empty string while the other platforms
            // leave the variable unset, so test with `${VAR:-}`. The key
            // reaches the file through a quoted expansion of the job's own
            // environment, never through argv.
            Step::run(
                "Set up SSH for the AUR",
                format!(
                    "if [ -z \"${{AUR_SSH_PRIVATE_KEY:-}}\" ]; then echo \"AUR_SSH_PRIVATE_KEY is not set; cannot push to the AUR\" >&2; exit 1; fi; \
                     mkdir -p {SSH_DIR}; \
                     printf '%s\\n' \"$AUR_SSH_PRIVATE_KEY\" > {SSH_DIR}/aur; \
                     ssh-keyscan aur.archlinux.org > {SSH_DIR}/known_hosts; \
                     printf 'Host aur.archlinux.org\\n  User aur\\n  IdentityFile {SSH_DIR}/aur\\n' > {SSH_DIR}/config; \
                     chmod 700 {SSH_DIR}; chmod 600 {SSH_DIR}/aur {SSH_DIR}/config {SSH_DIR}/known_hosts; \
                     chown -R builder:builder {SSH_DIR}"
                ),
            ),
            // The clone lives in builder's home, not the workspace: the
            // workspace is root-owned on every platform and has no business
            // inside the package build
            Step::run(
                format!("Clone {pkg} from the AUR"),
                format!(
                    "HOME=/home/builder runuser -u builder -- git clone ssh://aur@aur.archlinux.org/{pkg}.git {clone}"
                ),
            ),
            // `tr` keeps the tag from carrying sed metacharacters into the
            // PKGBUILD — the tag comes from whoever can push one
            Step::run(
                "Set pkgver from the git tag",
                format!(
                    "version=\"$(git describe --tags --exact-match)\"; \
                     version=\"$(printf '%s' \"${{version#v}}\" | tr -cd '0-9a-zA-Z._+')\"; \
                     HOME=/home/builder runuser -u builder -- sed -i \"s/^pkgver=.*/pkgver=$version/;s/^pkgrel=.*/pkgrel=1/\" {clone}/PKGBUILD"
                ),
            ),
            Step::run(
                "Update checksums",
                format!("cd {clone} && HOME=/home/builder runuser -u builder -- updpkgsums"),
            ),
            Step::run(
                "Build and lint the package",
                format!(
                    "cd {clone} && HOME=/home/builder runuser -u builder -- makepkg -sf --noconfirm && namcap PKGBUILD && namcap *.pkg.tar*"
                ),
            ),
            // The redirect happens in builder's shell so .SRCINFO is
            // builder-owned like the rest of the clone
            Step::run(
                "Regenerate .SRCINFO",
                format!(
                    "cd {clone} && HOME=/home/builder runuser -u builder -- sh -c 'makepkg --printsrcinfo > .SRCINFO'"
                ),
            ),
            // The commit message re-reads pkgver from the PKGBUILD because
            // shell variables don't survive across steps on GitHub
            Step::run(
                "Push to the AUR",
                format!(
                    "cd {clone} && HOME=/home/builder runuser -u builder -- sh -c 'git config user.name cibox && git config user.email cibox@localhost && git add PKGBUILD .SRCINFO && git commit -m \"Update to $(sed -n \"s/^pkgver=//p\" PKGBUILD)\" && git push origin master'"
                ),
            ),
        ]
    }
}

impl Rule for AurRelease {
    fn id(&self) -> &'static str {
        "aur-release"
    }

    fn owns_job_id(&self, id: &str) -> bool {
        // Variant ids carry user-chosen package names, so prune-time
        // ownership can only match the prefix — the current config no longer
        // knows the name of a removed package
        id == self.id() || id.starts_with("aur-release-")
    }

    fn name(&self) -> &'static str {
        "AUR publish"
    }

    fn description(&self) -> &'static str {
        "Publish to the AUR on version tags (requires AUR_SSH_PRIVATE_KEY)"
    }

    fn detect(&self, _facts: &ProjectFacts) -> bool {
        // The AUR repo holds the PKGBUILD, so there is nothing in the
        // project to detect; enable via cibox.ron
        false
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        match self.packages.as_slice() {
            // resolve() always injects at least one name, but jobs() must
            // work for a bare struct too
            [] => vec![self.release_job("", self.name(), crate::config::aur::FALLBACK_NAME)],
            [pkg] => vec![self.release_job("", self.name(), pkg)],
            pkgs => pkgs
                .iter()
                .map(|pkg| {
                    self.release_job(&format!("-{pkg}"), format!("AUR publish ({pkg})"), pkg)
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(packages: &[&str]) -> AurRelease {
        AurRelease {
            packages: packages.iter().map(|p| p.to_string()).collect(),
        }
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
    fn test_disabled_by_default() {
        assert!(!rule(&["app"]).detect(&ProjectFacts::default()));
    }

    #[test]
    fn test_single_package_job_shape() {
        let jobs = rule(&["cibox"]).jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "aur-release");
        assert_eq!(job.stage, Stage::Deploy);
        assert!(job.tags_only);
        assert_eq!(job.image.as_deref(), Some(IMAGE));
        assert!(!job.needs_docker);
        assert_eq!(job.secrets, vec!["AUR_SSH_PRIVATE_KEY"]);
        assert!(job.variables.is_empty());
    }

    #[test]
    fn test_jobs_without_packages_still_emit() {
        // The ProjectFacts::default() contract: force-enabled rules must
        // produce jobs even when resolve() never injected a name
        let jobs = rule(&[]).jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 1);
        assert!(run_command(&jobs[0], "Clone app from the AUR")
            .contains("ssh://aur@aur.archlinux.org/app.git"));
    }

    #[test]
    fn test_install_step_precedes_checkout() {
        // base-devel has no git: without it in the container,
        // actions/checkout falls back to a tarball with no .git and
        // `git describe` breaks
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        assert!(
            matches!(&job.steps[0], Step::Run { name, command } if name == "Install build tools"
                && command.contains("pacman -Syu")
                && command.contains(" git ")),
            "{:?}",
            job.steps[0]
        );
        assert!(matches!(&job.steps[1], Step::Checkout { .. }));
    }

    #[test]
    fn test_multi_package_emits_one_job_per_name() {
        let rule = rule(&["goldboot", "goldboot-registry"]);
        let jobs = rule.jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "aur-release-goldboot");
        assert_eq!(jobs[1].id, "aur-release-goldboot-registry");
        assert!(run_command(&jobs[0], "Clone goldboot from the AUR")
            .contains("ssh://aur@aur.archlinux.org/goldboot.git"));
        assert!(
            run_command(&jobs[1], "Clone goldboot-registry from the AUR")
                .contains("ssh://aur@aur.archlinux.org/goldboot-registry.git")
        );
        for job in &jobs {
            assert!(rule.owns_job_id(&job.id), "{}", job.id);
        }
        // Ownership must also cover a variant whose package was removed from
        // the knob, or `cibox update` could never prune it
        assert!(rule.owns_job_id("aur-release-gone"));
    }

    #[test]
    fn test_missing_key_fails_the_job() {
        // Unlike the docker login skip, a missing AUR key is always an
        // error: the rule only exists when explicitly enabled, and a skip
        // would silently drop releases
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        let ssh = run_command(job, "Set up SSH for the AUR");
        assert!(
            ssh.starts_with("if [ -z \"${AUR_SSH_PRIVATE_KEY:-}\" ]; then"),
            "{ssh}"
        );
        assert!(ssh.contains("exit 1"), "{ssh}");
    }

    #[test]
    fn test_key_never_in_argv() {
        // The key must reach its file through a quoted environment
        // expansion; `NAME=value` on a command line would put the private
        // key into argv, readable through /proc
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        for step in &job.steps {
            let Step::Run { command, .. } = step else {
                continue;
            };
            assert!(!command.contains("AUR_SSH_PRIVATE_KEY="), "{command}");
        }
        let ssh = run_command(job, "Set up SSH for the AUR");
        assert!(
            ssh.contains("printf '%s\\n' \"$AUR_SSH_PRIVATE_KEY\" > /home/builder/.ssh/aur"),
            "{ssh}"
        );
    }

    #[test]
    fn test_build_runs_as_non_root() {
        // makepkg refuses to run as root, and container jobs are root on
        // every platform
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        for step in &job.steps {
            let Step::Run { command, .. } = step else {
                continue;
            };
            if command.contains("makepkg") || command.contains("updpkgsums") {
                assert!(command.contains("runuser -u builder"), "{command}");
            }
        }
    }

    #[test]
    fn test_pkgver_strips_leading_v_and_resets_pkgrel() {
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        let step = run_command(job, "Set pkgver from the git tag");
        assert!(step.contains("git describe --tags --exact-match"), "{step}");
        assert!(step.contains("${version#v}"), "{step}");
        assert!(step.contains("s/^pkgrel=.*/pkgrel=1/"), "{step}");
        // The tag is attacker-adjacent input for the sed program
        assert!(step.contains("tr -cd '0-9a-zA-Z._+'"), "{step}");
    }

    #[test]
    fn test_srcinfo_regenerated_and_pushed() {
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        assert!(run_command(job, "Regenerate .SRCINFO").contains("makepkg --printsrcinfo"));
        let push = run_command(job, "Push to the AUR");
        assert!(push.contains("git add PKGBUILD .SRCINFO"), "{push}");
        assert!(push.contains("git push origin master"), "{push}");
    }

    #[test]
    fn test_full_build_and_lint_before_push() {
        let job = &rule(&["cibox"]).jobs(&ProjectFacts::default())[0];
        let build = run_command(job, "Build and lint the package");
        assert!(build.contains("makepkg -sf --noconfirm"), "{build}");
        assert!(build.contains("namcap PKGBUILD"), "{build}");
        assert!(build.contains("namcap *.pkg.tar*"), "{build}");
        // Lint must come before the push in step order
        let position = |name: &str| {
            job.steps
                .iter()
                .position(|s| matches!(s, Step::Run { name: n, .. } if n == name))
                .unwrap()
        };
        assert!(position("Build and lint the package") < position("Push to the AUR"));
    }
}
