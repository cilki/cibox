use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubWorkflow {
    pub name: String,
    pub on: GitHubTriggers,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    pub jobs: BTreeMap<String, GitHubJob>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum GitHubTriggers {
    Simple(Vec<String>),
    Detailed(BTreeMap<String, GitHubTriggerConfig>),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubTriggerConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubJob {
    #[serde(rename = "runs-on")]
    pub runs_on: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    pub steps: Vec<GitHubStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<Vec<String>>,
    #[serde(rename = "timeout-minutes", skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
    #[serde(rename = "continue-on-error", skip_serializing_if = "Option::is_none")]
    pub continue_on_error: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubStep {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uses: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub with: Option<BTreeMap<String, serde_yaml::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_workflow() {
        let workflow = GitHubWorkflow {
            name: "CI".to_string(),
            on: GitHubTriggers::Simple(vec!["push".to_string()]),
            env: None,
            jobs: BTreeMap::from([(
                "test".to_string(),
                GitHubJob {
                    runs_on: "ubuntu-latest".to_string(),
                    container: None,
                    env: None,
                    steps: vec![GitHubStep {
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
        assert!(yaml.contains("name: CI"));
        assert!(yaml.contains("ubuntu-latest"));
    }
}
