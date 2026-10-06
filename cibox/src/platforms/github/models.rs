use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubWorkflow {
    pub name: String,
    /// Trigger event name -> the refs it fires on
    pub on: BTreeMap<String, GitHubTriggerConfig>,
    /// Scopes granted to the ambient CI token for every job that doesn't
    /// override it. Omitting this inherits the repository/organization
    /// default, which may be write-all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<BTreeMap<String, String>>,
    pub jobs: BTreeMap<String, GitHubJob>,
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
    /// Display name; set for matrix jobs so each leg shows its version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "runs-on")]
    pub runs_on: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strategy: Option<GitHubStrategy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub defaults: Option<GitHubDefaults>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub container: Option<String>,
    /// Replaces the workflow-level scopes for this job alone
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<BTreeMap<String, String>>,
    pub steps: Vec<GitHubStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<Vec<String>>,
    #[serde(rename = "timeout-minutes", skip_serializing_if = "Option::is_none")]
    pub timeout_minutes: Option<u32>,
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    pub if_expr: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubStrategy {
    /// One failing toolchain must not cancel the other matrix legs
    #[serde(rename = "fail-fast")]
    pub fail_fast: bool,
    pub matrix: GitHubMatrix,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubMatrix {
    pub include: Vec<GitHubMatrixInclude>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubMatrixInclude {
    pub version: String,
    pub image: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubDefaults {
    pub run: GitHubRunDefaults,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitHubRunDefaults {
    pub shell: String,
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
}
