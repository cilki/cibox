//! Per-platform backends: typed models for each CI format plus the lowering
//! from the platform-neutral IR (`crate::ir`) onto them.
//!
//! Jobs arrive with their final keys (the rule ids); trigger rules and other
//! platform idioms are owned by these backends. Gitea Actions uses the
//! GitHub Actions workflow format, so it has no backend of its own.

pub mod circleci;
pub mod github;
pub mod gitlab;
