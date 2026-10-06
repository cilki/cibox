use super::Rule;
use crate::detection::ProjectFacts;
use crate::ir::{Job, Stage, Step};

/// --redact keeps secrets out of CI logs
const SCAN_COMMAND: &str = "gitleaks detect --source . --redact -v";

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
            .with_image(super::CIBOX_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::checkout_full_history(),
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
        assert_eq!(jobs[0].image.as_deref(), Some(super::super::CIBOX_IMAGE));
        assert!(jobs[0]
            .steps
            .contains(&Step::Checkout { full_history: true }));
    }
}
