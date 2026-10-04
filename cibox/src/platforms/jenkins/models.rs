// Jenkins uses Groovy syntax, not YAML, so we'll generate it as a string template
// This is a simplified representation

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsConfig {
    pub agent: String,
    pub environment: Vec<(String, String)>,
    pub stages: Vec<JenkinsStage>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct JenkinsStage {
    pub name: String,
    pub steps: Vec<String>,
    /// Restrict the stage to tag builds matching this pattern (`when { tag }`)
    pub when_tag: Option<String>,
}
