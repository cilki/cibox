use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIConfig {
    pub version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orbs: Option<BTreeMap<String, String>>,
    pub jobs: BTreeMap<String, CircleCIJob>,
    pub workflows: BTreeMap<String, CircleCIWorkflow>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIJob {
    pub docker: Vec<CircleCIDocker>,
    pub steps: Vec<CircleCIStep>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<BTreeMap<String, String>>,
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
    StoreArtifacts {
        #[serde(rename = "store_artifacts")]
        store_artifacts: CircleCIStoreArtifacts,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CircleCIStoreArtifacts {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CircleCIRun {
    Simple(String),
    Detailed { name: String, command: String },
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filters: Option<CircleCIFilters>,
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
