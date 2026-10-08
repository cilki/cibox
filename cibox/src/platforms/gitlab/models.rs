use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabCI {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<GitLabCache>,
    #[serde(flatten)]
    pub jobs: BTreeMap<String, GitLabJob>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabJob {
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// The job's environment. GitLab has no job-level `env:`, so `variables:`
    /// is where it goes; set at the top level instead it would apply to every
    /// other job in the pipeline too.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variables: Option<BTreeMap<String, String>>,
    pub script: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_script: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_script: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub needs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<GitLabCache>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<GitLabArtifacts>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rules: Option<Vec<GitLabRule>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel: Option<GitLabParallel>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabParallel {
    pub matrix: Vec<GitLabMatrixCell>,
}

/// One matrix combination. A struct rather than a map so VERSION serializes
/// first — GitLab derives the leg's display name from the variable order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabMatrixCell {
    #[serde(rename = "VERSION")]
    pub version: String,
    #[serde(rename = "IMAGE")]
    pub image: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabCache {
    pub key: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabArtifacts {
    pub paths: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// One `rules:` entry. `only:refs` can express "tags" or a ref regex but not
/// both at once (a regex there matches branch names too), so release gating
/// needs a `rules:` condition on `$CI_COMMIT_TAG`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitLabRule {
    #[serde(rename = "if")]
    pub if_expr: String,
}
