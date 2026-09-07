use crate::ir::{Job, Stage, Step, ToJobs};
use cibox_macros::Preset;

/// Command shared by all platforms; --redact keeps secrets out of CI logs
const SCAN_COMMAND: &str = "gitleaks detect --source . --redact -v";

/// Secret scanning with Gitleaks, running on the cibox container
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, Preset)]
#[preset(category = "Security")]
#[serde(default)]
pub struct Gitleaks {
    /// Scan the repository for hardcoded secrets
    #[preset_field(display = "Secret Scan")]
    pub(super) enable_scan: bool,

    /// Scan full git history, not just the latest commit
    #[preset_field(display = "Full History")]
    pub(super) scan_full_history: bool,
}

impl ToJobs for Gitleaks {
    fn jobs(&self) -> Vec<Job> {
        if !self.enable_scan {
            return Vec::new();
        }

        vec![Job::new("scan", "Secret Scan", Stage::Security)
            .with_image(crate::presets::CIBOX_IMAGE)
            .with_timeout(10)
            .with_steps(vec![
                Step::Checkout {
                    full_history: self.scan_full_history,
                },
                Step::run("Run gitleaks", SCAN_COMMAND),
            ])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default() {
        let preset = Gitleaks::default();
        assert!(!preset.enable_scan);
        assert!(!preset.scan_full_history);
    }

    #[test]
    fn test_disabled_produces_no_jobs() {
        assert!(Gitleaks::default().jobs().is_empty());
    }

    #[test]
    fn test_scan_job() {
        let preset = Gitleaks {
            enable_scan: true,
            scan_full_history: true,
        };
        let jobs = preset.jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "scan");
        assert_eq!(jobs[0].image.as_deref(), Some(crate::presets::CIBOX_IMAGE));
        assert!(jobs[0]
            .steps
            .contains(&Step::Checkout { full_history: true }));
        assert!(jobs[0]
            .steps
            .iter()
            .any(|s| matches!(s, Step::Run { command, .. } if command.contains("gitleaks detect"))));
    }
}
