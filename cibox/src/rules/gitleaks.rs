use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// --redact keeps secrets out of CI logs
const SCAN_COMMAND: &str = "gitleaks detect --source . --redact -v";

/// The gitleaks release the job installs; the checksums are from the
/// release's gitleaks_<version>_checksums.txt
const GITLEAKS_VERSION: &str = "8.30.1";
const GITLEAKS_SHA256_X64: &str =
    "551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb";
const GITLEAKS_SHA256_ARM64: &str =
    "e4a487ee7ccd7d3a7f7ec08657610aa3606637dab924210b3aee62570fb4b080";

/// The official gitleaks image sets `gitleaks` as its entrypoint, which
/// GitLab and CircleCI would run in place of the job's shell, so the release
/// binary is installed into DEBIAN_IMAGE instead
fn install_gitleaks() -> Step {
    Step::run(
        "Install gitleaks",
        format!(
            "case \"$(uname -m)\" in \
             x86_64) arch=x64; sum={GITLEAKS_SHA256_X64} ;; \
             aarch64) arch=arm64; sum={GITLEAKS_SHA256_ARM64} ;; \
             *) echo \"No gitleaks build for $(uname -m)\" >&2; exit 1 ;; esac; \
             curl -fsSL \"https://github.com/gitleaks/gitleaks/releases/download/v{GITLEAKS_VERSION}/gitleaks_{GITLEAKS_VERSION}_linux_$arch.tar.gz\" -o /tmp/gitleaks.tar.gz && \
             echo \"$sum  /tmp/gitleaks.tar.gz\" | sha256sum -c - && \
             tar -xzf /tmp/gitleaks.tar.gz -C /usr/local/bin gitleaks && \
             rm /tmp/gitleaks.tar.gz"
        ),
    )
}

/// Scan the full git history for hardcoded secrets
pub struct Gitleaks;

impl Rule for Gitleaks {
    fn id(&self) -> &'static str {
        "gitleaks"
    }

    fn name(&self) -> &'static str {
        "Gitleaks"
    }

    fn description(&self) -> &'static str {
        "Scan the full git history for hardcoded secrets"
    }

    fn detect(&self, _facts: &ProjectFacts) -> bool {
        // Off by default
        false
    }

    fn jobs(&self, _facts: &ProjectFacts) -> Vec<Job> {
        vec![Job::new(self.id(), self.name(), Stage::Security)
            .with_image(super::DEBIAN_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout_full_history(),
                install_gitleaks(),
                Step::run("Run gitleaks", SCAN_COMMAND),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_by_default() {
        assert!(!Gitleaks.detect(&ProjectFacts::default()));
        let facts = ProjectFacts {
            is_git_repo: true,
            ..ProjectFacts::default()
        };
        assert!(!Gitleaks.detect(&facts));
    }

    #[test]
    fn test_scan_job() {
        let jobs = Gitleaks.jobs(&ProjectFacts::default());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "gitleaks");
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::DEBIAN_IMAGE));
        assert!(jobs[0]
            .steps
            .contains(&Step::Checkout { full_history: true }));
        // DEBIAN_IMAGE has no gitleaks: it is installed before the scan
        let install = jobs[0].steps.iter().position(|s| *s == install_gitleaks());
        let scan = jobs[0]
            .steps
            .iter()
            .position(|s| matches!(s, Step::Run { command, .. } if command == SCAN_COMMAND));
        assert!(install.is_some() && install < scan, "{:?}", jobs[0].steps);
    }
}
