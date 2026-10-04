use crate::ir::{Job, Step};
use crate::platforms::jenkins::models::{JenkinsConfig, JenkinsStage};

pub fn lower_jenkins(jobs: &[Job]) -> JenkinsConfig {
    let mut environment = Vec::new();
    let mut stages = Vec::new();

    for job in jobs {
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
            name: job.name.clone(),
            steps,
            when_tag: job.tags_only.then(|| "v*".to_string()),
        });
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
        let config = lower_jenkins(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_steps(vec![Step::checkout(), Step::run("Test", "cargo test")])]);
        assert_eq!(config.stages[0].name, "Cargo test");
        assert_eq!(config.stages[0].steps, vec!["sh 'cargo test'"]);
        assert_eq!(config.stages[0].when_tag, None);
    }

    #[test]
    fn test_tags_only_sets_when_tag() {
        let config = lower_jenkins(&[Job::new("rust-release", "Cargo publish", Stage::Deploy)
            .tags_only()
            .with_steps(vec![Step::run("Publish", "cargo publish")])]);
        assert_eq!(config.stages[0].when_tag.as_deref(), Some("v*"));
    }

    #[test]
    fn test_containerized_job_uses_docker_run() {
        let config = lower_jenkins(&[Job::new("gitleaks", "Gitleaks", Stage::Security)
            .with_image("fossable/cibox:latest")
            .with_steps(vec![
                Step::checkout(),
                Step::run("Scan", "gitleaks detect --source . --redact -v"),
            ])]);
        assert_eq!(
            config.stages[0].steps,
            vec![
                "sh 'docker run --rm -v \"$WORKSPACE:/w\" -w /w fossable/cibox:latest bash -c \"gitleaks detect --source . --redact -v\"'"
            ]
        );
    }

    #[test]
    fn test_single_quotes_escaped() {
        let config = lower_jenkins(&[Job::new("rust-test", "Cargo test", Stage::Test)
            .with_steps(vec![Step::run("Echo", "echo 'hi'")])]);
        assert_eq!(config.stages[0].steps, vec!["sh 'echo \\'hi\\''"]);
    }
}
