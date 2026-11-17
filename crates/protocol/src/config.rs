//! Global `~/.analyst/config.toml` and project `<repo>/.analyst/project.toml` (PROTOCOL 2.1, 2.2).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

// ---- global config ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct GlobalConfig {
    pub defaults: Defaults,
    pub providers: Vec<ProviderConfig>,
    pub models: Vec<ModelConfig>,
    pub targets: Vec<TargetConfig>,
    /// Where Python packages are installed from.
    pub packages: PackagesConfig,
    /// Interpreter of the app tooling environment (hosts `analyst_kernel`); optional.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tooling_python: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Defaults {
    /// `"<provider name>/<model id>"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub target: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Defaults {
            model: None,
            target: "local".to_string(),
        }
    }
}

/// The default Python package index (`https://pypi.org/simple`).
pub const DEFAULT_PACKAGE_INDEX: &str = "https://pypi.org/simple";

/// `[packages]`: the one package index installs may contact (public PyPI or a mirror).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct PackagesConfig {
    /// Simple-API index URL handed to `uv` for every install.
    pub index_url: String,
}

impl Default for PackagesConfig {
    fn default() -> Self {
        PackagesConfig {
            index_url: DEFAULT_PACKAGE_INDEX.to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
pub enum ProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    #[serde(rename = "anthropic")]
    Anthropic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub name: String,
    pub kind: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
}

impl ProviderConfig {
    /// The effective base URL: the configured one or the provider kind's default.
    pub fn effective_base_url(&self) -> String {
        match (&self.base_url, self.kind) {
            (Some(u), _) => u.trim_end_matches('/').to_string(),
            (None, ProviderKind::Anthropic) => "https://api.anthropic.com".to_string(),
            (None, ProviderKind::OpenAiCompatible) => "http://localhost:11434/v1".to_string(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ToolSupport {
    /// Native tool calling: required.
    Native,
    /// No native tool calling: the model is rejected. `text-protocol` (a fallback tier that
    /// no longer exists) is read as this value so old files still load.
    #[serde(alias = "text-protocol")]
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub provider: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<ToolSupport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vision: Option<bool>,
    /// Output token limit per model call (`max_tokens`); default 16384.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
}

/// Default `max_tokens` when a model has no `max_output_tokens` override (PROTOCOL 9).
pub const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 16384;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "lowercase")]
pub enum TargetKind {
    #[default]
    Local,
    Ssh,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TargetConfig {
    pub name: String,
    #[serde(default)]
    pub kind: TargetKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<String>,
    #[serde(default)]
    pub data_dirs: Vec<String>,
    /// A local `uv` binary built for this host's OS and architecture. It is copied to the
    /// host when the host has no `uv` and the local `uv` does not match its platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uv_binary: Option<String>,
}

// ---- project config ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(from = "ProjectConfigRaw", into = "ProjectConfigRaw")]
pub struct ProjectConfig {
    pub project: ProjectSection,
    pub contract: ContractConfig,
    pub policy: PolicyConfig,
    /// `policy.unsandboxed_runs` in the file (PROTOCOL 9). Kept beside `policy` rather than
    /// inside [`PolicyConfig`] so a session policy override can never loosen it.
    pub unsandboxed_runs: UnsandboxedRuns,
    /// Per-target overrides keyed by target name.
    pub targets: BTreeMap<String, TargetOverride>,
}

/// What run-class tools do when the kernel reports sandbox `limits` or `none`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "lowercase")]
pub enum UnsandboxedRuns {
    #[default]
    Ask,
    Auto,
}

/// On-disk shape of `[policy]`. A `network = ...` key from older files is accepted and
/// ignored: sandboxed code never has network access, so there is nothing to configure.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
struct PolicyTable {
    #[serde(flatten)]
    base: PolicyConfig,
    unsandboxed_runs: UnsandboxedRuns,
}

/// On-disk shape of `project.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
struct ProjectConfigRaw {
    project: ProjectSection,
    contract: ContractConfig,
    policy: PolicyTable,
    targets: BTreeMap<String, TargetOverride>,
}

impl From<ProjectConfigRaw> for ProjectConfig {
    fn from(r: ProjectConfigRaw) -> Self {
        ProjectConfig {
            project: r.project,
            contract: r.contract,
            policy: r.policy.base,
            unsandboxed_runs: r.policy.unsandboxed_runs,
            targets: r.targets,
        }
    }
}

impl From<ProjectConfig> for ProjectConfigRaw {
    fn from(c: ProjectConfig) -> Self {
        ProjectConfigRaw {
            project: c.project,
            contract: c.contract,
            policy: PolicyTable {
                base: c.policy,
                unsandboxed_runs: c.unsandboxed_runs,
            },
            targets: c.targets,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ProjectSection {
    pub scripts: Vec<String>,
    pub library: String,
    pub out_dir: String,
    pub data_dirs: Vec<String>,
    pub timeout_s: u64,
}

impl Default for ProjectSection {
    fn default() -> Self {
        ProjectSection {
            scripts: vec!["scripts/**/*.py".to_string()],
            library: "analysis".to_string(),
            out_dir: "out".to_string(),
            data_dirs: vec!["data".to_string()],
            timeout_s: 600,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ContractConfig {
    pub max_helper_lines: u32,
    pub near_duplicate_threshold: f64,
    pub min_tokens_for_similarity: u32,
    pub math_required: Vec<String>,
    pub numerical_modules: Vec<String>,
    pub script_allowed_functions: Vec<String>,
    /// Rule id -> `"error"` | `"warn"`.
    pub severity: BTreeMap<String, String>,
}

impl Default for ContractConfig {
    fn default() -> Self {
        ContractConfig {
            max_helper_lines: 15,
            near_duplicate_threshold: 0.80,
            min_tokens_for_similarity: 30,
            math_required: vec![],
            numerical_modules: [
                "numpy",
                "scipy",
                "pandas",
                "sklearn",
                "jax",
                "torch",
                "xarray",
                "statsmodels",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            script_allowed_functions: ["main", "build_parser", "parse_args"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            severity: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "kebab-case")]
pub enum EditPolicy {
    #[default]
    Review,
    AutoNewFiles,
    Auto,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "lowercase")]
pub enum RunPolicy {
    #[default]
    Auto,
    Ask,
}

/// `ask | auto | deny` for packages, shell, git.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, TS)]
#[serde(rename_all = "lowercase")]
pub enum AskPolicy {
    #[default]
    Ask,
    Auto,
    Deny,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct PolicyConfig {
    pub edits: EditPolicy,
    pub runs: RunPolicy,
    pub packages: AskPolicy,
    pub shell: AskPolicy,
    pub git: AskPolicy,
    pub protected: Vec<String>,
    pub max_steps: u32,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        PolicyConfig {
            edits: EditPolicy::Review,
            runs: RunPolicy::Auto,
            packages: AskPolicy::Ask,
            shell: AskPolicy::Ask,
            git: AskPolicy::Ask,
            protected: vec![],
            max_steps: 40,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct TargetOverride {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data_dirs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_s: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub out_dir: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_config_parses_documented_example() {
        let text = r#"
[defaults]
model = "anthropic/claude-x"
target = "local"

[[providers]]
name = "ollama-lab"
kind = "openai-compatible"
base_url = "http://lab-gpu:11434/v1"
api_key_env = "OLLAMA_LAB_API_KEY"

[[providers]]
name = "anthropic"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"

[[models]]
provider = "ollama-lab"
id = "some-model"
context_window = 65536
tools = "native"
vision = false

[[targets]]
name = "lab-gpu"
kind = "ssh"
host = "lab-gpu"
remote_root = "~/analyst"
python = "3.12"
sandbox = "auto"
data_dirs = ["/data/shared"]
"#;
        let c: GlobalConfig = toml::from_str(text).unwrap();
        assert_eq!(c.defaults.model.as_deref(), Some("anthropic/claude-x"));
        assert_eq!(c.providers.len(), 2);
        assert_eq!(c.providers[1].kind, ProviderKind::Anthropic);
        assert_eq!(
            c.providers[1].effective_base_url(),
            "https://api.anthropic.com"
        );
        assert_eq!(c.models[0].tools, Some(ToolSupport::Native));
        assert_eq!(c.targets[0].kind, TargetKind::Ssh);
        assert_eq!(c.packages.index_url, DEFAULT_PACKAGE_INDEX);
        assert_eq!(c.targets[0].uv_binary, None);
        let s = toml::to_string(&c).unwrap();
        let back: GlobalConfig = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
        let mirror: GlobalConfig = toml::from_str(
            "[packages]\nindex_url = \"https://mirror.corp/simple\"\n\n[[targets]]\nname = \"hpc\"\nkind = \"ssh\"\nhost = \"hpc\"\nuv_binary = \"/opt/uv/aarch64/uv\"\n",
        )
        .unwrap();
        assert_eq!(mirror.packages.index_url, "https://mirror.corp/simple");
        assert_eq!(
            mirror.targets[0].uv_binary.as_deref(),
            Some("/opt/uv/aarch64/uv")
        );
    }

    /// Older project files carry `network = "deny" | "ask" | "allow"`. The key is gone; the
    /// file still loads and nothing in the loaded policy can grant network access.
    #[test]
    fn a_leftover_network_policy_is_ignored() {
        for value in ["deny", "ask", "allow"] {
            let c: ProjectConfig = toml::from_str(&format!(
                "[policy]\nruns = \"ask\"\nnetwork = \"{value}\"\n"
            ))
            .unwrap();
            assert_eq!(c.policy.runs, RunPolicy::Ask);
            assert!(!toml::to_string(&c).unwrap().contains("network"));
        }
    }

    #[test]
    fn project_config_defaults_match_doc() {
        let c: ProjectConfig = toml::from_str("").unwrap();
        assert_eq!(c, ProjectConfig::default());
        assert_eq!(c.project.scripts, vec!["scripts/**/*.py"]);
        assert_eq!(c.project.library, "analysis");
        assert_eq!(c.project.out_dir, "out");
        assert_eq!(c.project.timeout_s, 600);
        assert_eq!(c.contract.max_helper_lines, 15);
        assert!((c.contract.near_duplicate_threshold - 0.8).abs() < 1e-9);
        assert_eq!(c.policy.edits, EditPolicy::Review);
        assert_eq!(c.policy.runs, RunPolicy::Auto);
        assert_eq!(c.policy.max_steps, 40);
    }

    #[test]
    fn project_config_parses_documented_example() {
        let text = r#"
[project]
scripts = ["scripts/**/*.py"]
library = "analysis"
out_dir = "out"
data_dirs = ["data"]
timeout_s = 600

[contract]
max_helper_lines = 15
near_duplicate_threshold = 0.80
math_required = []

[contract.severity]
"lib.near-duplicate" = "warn"

[policy]
edits = "auto-new-files"
runs = "ask"
packages = "deny"
protected = ["analysis/core/**"]
max_steps = 12

[targets.lab-gpu]
data_dirs = ["/data/shared/run3"]
"#;
        let c: ProjectConfig = toml::from_str(text).unwrap();
        assert_eq!(c.policy.edits, EditPolicy::AutoNewFiles);
        assert_eq!(c.policy.runs, RunPolicy::Ask);
        assert_eq!(c.policy.packages, AskPolicy::Deny);
        assert_eq!(c.policy.max_steps, 12);
        assert_eq!(c.unsandboxed_runs, UnsandboxedRuns::Ask);
        let auto: ProjectConfig =
            toml::from_str("[policy]\nunsandboxed_runs = \"auto\"\nruns = \"ask\"\n").unwrap();
        assert_eq!(auto.unsandboxed_runs, UnsandboxedRuns::Auto);
        assert_eq!(auto.policy.runs, RunPolicy::Ask);
        assert!(toml::to_string(&auto)
            .unwrap()
            .contains("unsandboxed_runs = \"auto\""));
        assert_eq!(c.contract.severity["lib.near-duplicate"], "warn");
        assert_eq!(
            c.targets["lab-gpu"].data_dirs.as_deref(),
            Some(&["/data/shared/run3".to_string()][..])
        );
        let s = toml::to_string(&c).unwrap();
        let back: ProjectConfig = toml::from_str(&s).unwrap();
        assert_eq!(c, back);
    }
}
