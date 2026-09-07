use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[strum(ascii_case_insensitive)]
pub enum Platform {
    GitHub,
    Gitea,
    GitLab,
    CircleCI,
    Jenkins,
}

impl Platform {
    pub fn all() -> Vec<Platform> {
        vec![
            Platform::GitHub,
            Platform::Gitea,
            Platform::GitLab,
            Platform::CircleCI,
            Platform::Jenkins,
        ]
    }

    pub fn name(&self) -> &'static str {
        match self {
            Platform::GitHub => "GitHub Actions",
            Platform::Gitea => "Gitea Actions",
            Platform::GitLab => "GitLab CI",
            Platform::CircleCI => "CircleCI",
            Platform::Jenkins => "Jenkins",
        }
    }

    pub fn output_path(&self) -> PathBuf {
        match self {
            Platform::GitHub => PathBuf::from(".github/workflows/ci.yml"),
            Platform::Gitea => PathBuf::from(".gitea/workflows/ci.yml"),
            Platform::GitLab => PathBuf::from(".gitlab-ci.yml"),
            Platform::CircleCI => PathBuf::from(".circleci/config.yml"),
            Platform::Jenkins => PathBuf::from("Jenkinsfile"),
        }
    }
}
