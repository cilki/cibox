use crate::error::Result;
use crate::platforms::gitlab::models::GitLabCI;
use crate::traits::ToGitLab;

use super::PythonApp;

impl ToGitLab for PythonApp {
    fn to_gitlab(&self) -> Result<GitLabCI> {
        use crate::platforms::gitlab::models::*;
        use std::collections::BTreeMap;

        let mut jobs = BTreeMap::new();

        let mut script = vec![
            "pip install -r requirements.txt".to_string(),
            "pytest".to_string(),
        ];

        if let Some(linter) = &self.linter {
            script.insert(1, format!("pip install {}", linter.name()));
            script.insert(2, linter.check_command().to_string());
        }

        if self.enable_type_check {
            script.insert(1, "pip install mypy".to_string());
            script.insert(2, "mypy .".to_string());
        }

        if let Some(formatter) = &self.formatter {
            script.insert(1, format!("pip install {}", formatter.name()));
            script.insert(2, formatter.check_command().to_string());
        }

        jobs.insert(
            "python-test".to_string(),
            GitLabJob {
                stage: "test".to_string(),
                image: Some(format!("python:{}", self.python_version)),
                script,
                before_script: None,
                after_script: None,
                needs: None,
                cache: None,
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
