use serde::Deserialize;
use serde::Serialize;

const fn default_bool<const VALUE: bool>() -> bool {
    VALUE
}

/// Specifies a replacement rule
#[derive(Debug, Serialize, Deserialize)]
pub struct Matcher {
    #[serde(default)]
    pub name: String,
    pub hosts: Vec<String>,
    #[serde(default = "default_bool::<true>")]
    pub terminates_matching: bool,
    #[serde(default)]
    pub param_matchers: Vec<Param>,
    #[serde(default)]
    pub path_matchers: Vec<PathComponent>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(default)]
    pub operation: ReplacementOperation,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PathComponent {
    pub name: String,
    #[serde(default)]
    pub operation: ReplacementOperation,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ReplacementOperation {
    #[default]
    Drop,
    ReplaceWith(String),
    RequestRedirect,
}
