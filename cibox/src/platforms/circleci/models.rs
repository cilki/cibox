use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIConfig {
    pub version: String,
    pub jobs: BTreeMap<String, CircleCIJob>,
    pub workflows: BTreeMap<String, CircleCIWorkflow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIJob {
    /// Declared inputs for a matrix job; referenced as << parameters.x >>
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<BTreeMap<String, CircleCIParameter>>,
    pub docker: Vec<CircleCIDocker>,
    pub steps: Vec<CircleCIStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIParameter {
    #[serde(rename = "type")]
    pub param_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIDocker {
    pub image: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CircleCIStep {
    Simple(String),
    Command {
        run: CircleCIRun,
    },
    Cache {
        #[serde(rename = "restore_cache")]
        restore_cache: CircleCICache,
    },
    SaveCache {
        #[serde(rename = "save_cache")]
        save_cache: CircleCICacheSave,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIRun {
    pub name: String,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCICache {
    pub keys: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCICacheSave {
    pub key: String,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIWorkflow {
    pub jobs: Vec<CircleCIWorkflowJob>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CircleCIWorkflowJob {
    Simple(String),
    Detailed {
        #[serde(flatten)]
        job: BTreeMap<String, CircleCIWorkflowJobDetail>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIWorkflowJobDetail {
    /// Distinct display name; required when one job is invoked several
    /// times (once per matrix leg)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<CircleCIFilters>,
    /// Parameter values for this invocation, inlined next to the other keys
    #[serde(flatten)]
    pub params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIFilters {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<CircleCIFilterPattern>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<CircleCIFilterPattern>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIFilterPattern {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub only: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<String>,
}
