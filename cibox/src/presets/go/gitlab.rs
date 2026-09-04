use crate::error::Result;
use crate::platforms::gitlab::models::GitLabCI;
use crate::traits::ToGitLab;

use super::GoApp;

impl ToGitLab for GoApp {
    fn to_gitlab(&self) -> Result<GitLabCI> {
        use crate::platforms::gitlab::models::*;
        use std::collections::BTreeMap;

        let mut jobs = BTreeMap::new();

        let mut script = vec!["go test -v ./...".to_string()];

        if self.enable_linter {
            script.insert(0, "golangci-lint run".to_string());
        }

        if self.enable_security_scan {
            script.insert(0, "gosec ./...".to_string());
        }

        jobs.insert(
            "go-test".to_string(),
            GitLabJob {
                stage: "test".to_string(),
                image: Some(format!("golang:{}", self.go_version)),
                script,
                before_script: None,
                after_script: None,
                needs: None,
                cache: Some(GitLabCache {
                    key: "go-cache".to_string(),
                    paths: vec!["~/go/pkg/mod".to_string()],
                }),
                artifacts: None,
                only: None,
                timeout: None,
            },
        );

        Ok(GitLabCI {
            stages: Some(vec!["test".to_string()]),
            variables: None,
            cache: None,
            jobs,
        })
    }
}
