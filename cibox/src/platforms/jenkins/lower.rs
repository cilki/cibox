use crate::ir::{PresetJobs, Step};
use crate::platforms::jenkins::models::{JenkinsConfig, JenkinsStage};

pub fn lower_jenkins(presets: &[PresetJobs]) -> JenkinsConfig {
    let multiple_presets = presets.len() > 1;
    let mut environment = Vec::new();
    let mut stages = Vec::new();

    for preset in presets {
        for job in &preset.jobs {
            for (key, value) in &job.env {
                if !environment.iter().any(|(k, _): &(String, String)| k == key) {
                    environment.push((key.clone(), value.clone()));
                }
            }

            let commands: Vec<&str> = job
                .steps
                .iter()
                .filter_map(|step| match step {
                    // Checkout is implicit on Jenkins
                    Step::Checkout { .. } => None,
                    Step::Run { command, .. } => Some(command.as_str()),
                })
                .collect();

            // The merged pipeline shares one agent, so containerized jobs run
            // their commands through `docker run` instead of a docker agent
            let steps = match (&job.image, job.needs_docker) {
                (Some(image), false) => {
                    vec![format!(
                        "sh 'docker run --rm -v \"$WORKSPACE:/w\" -w /w {image} bash -c \"{}\"'",
                        escape_groovy(&commands.join(" && ")).replace('"', "\\\"")
                    )]
                }
                _ => commands
                    .iter()
                    .map(|command| format!("sh '{}'", escape_groovy(command)))
                    .collect(),
            };

            stages.push(JenkinsStage {
                name: if multiple_presets {
                    format!("{}: {}", preset.display_name, job.name)
                } else {
                    job.name.clone()
                },
                steps,
            });
        }
    }

    JenkinsConfig {
        agent: "any".to_string(),
        environment,
        stages,
    }
}

/// Escape a command for embedding in a Groovy single-quoted string
fn escape_groovy(command: &str) -> String {
    command.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{Job, Stage, Step};

    #[test]
    fn test_commands_wrapped_in_sh() {
        let config = lower_jenkins(&[PresetJobs::new(
            "Rust",
            "Rust",
            vec![Job::new("test", "Test", Stage::Test)
                .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])],
        )]);
        assert_eq!(config.stages[0].name, "Test");
        assert_eq!(config.stages[0].steps, vec!["sh 'cargo test'"]);
    }

    #[test]
    fn test_stage_names_prefixed_with_multiple_presets() {
        let config = lower_jenkins(&[
            PresetJobs::new("Rust", "Rust", vec![Job::new("test", "Test", Stage::Test)]),
            PresetJobs::new(
                "Docker",
                "Docker",
                vec![Job::new("build", "Build", Stage::Build)],
            ),
        ]);
        assert_eq!(config.stages[0].name, "Rust: Test");
        assert_eq!(config.stages[1].name, "Docker: Build");
    }

    #[test]
    fn test_containerized_job_uses_docker_run() {
        let config = lower_jenkins(&[PresetJobs::new(
            "Gitleaks",
            "Gitleaks",
            vec![Job::new("scan", "Secret Scan", Stage::Security)
                .with_image("fossable/cibox:latest")
                .with_steps(vec![
                    Step::checkout(),
                    Step::run("Scan", "gitleaks detect --source . --redact -v"),
                ])],
        )]);
        assert_eq!(
            config.stages[0].steps,
            vec![
                "sh 'docker run --rm -v \"$WORKSPACE:/w\" -w /w fossable/cibox:latest bash -c \"gitleaks detect --source . --redact -v\"'"
            ]
        );
    }

    #[test]
    fn test_single_quotes_escaped() {
        let config = lower_jenkins(&[PresetJobs::new(
            "Rust",
            "Rust",
            vec![Job::new("test", "Test", Stage::Test)
                .with_steps(vec![Step::run("Echo", "echo 'hi'")])],
        )]);
        assert_eq!(config.stages[0].steps, vec!["sh 'echo \\'hi\\''"]);
    }
}
