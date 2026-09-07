// Gitea Actions is compatible with GitHub Actions workflow syntax
// We re-export GitHub Actions models with Gitea-specific type aliases

pub use crate::platforms::github::{
    GitHubJob as GiteaJob, GitHubStep as GiteaStep, GitHubTriggerConfig as GiteaTriggerConfig,
    GitHubTriggers as GiteaTriggers, GitHubWorkflow as GiteaWorkflow,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn test_serialize_gitea_workflow() {
        // Test that Gitea workflow (which is just a GitHub workflow) serializes correctly
        let workflow = GiteaWorkflow {
            name: "Gitea CI".to_string(),
            on: GiteaTriggers::Simple(vec!["push".to_string()]),
            env: None,
            jobs: BTreeMap::from([(
                "test".to_string(),
                GiteaJob {
                    runs_on: "ubuntu-latest".to_string(),
                    container: None,
                    env: None,
                    steps: vec![GiteaStep {
                        name: Some("Checkout".to_string()),
                        uses: Some("actions/checkout@v4".to_string()),
                        run: None,
                        with: None,
                        env: None,
                    }],
                    needs: None,
                    timeout_minutes: None,
                    continue_on_error: None,
                },
            )]),
        };

        let yaml = serde_yaml::to_string(&workflow).unwrap();
        assert!(yaml.contains("name: Gitea CI"));
        assert!(yaml.contains("ubuntu-latest"));
        assert!(yaml.contains("actions/checkout@v4"));
    }

}
