use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use serde::{Deserialize, Serialize};

use crate::model::FileCategory;

#[derive(Parser, Debug)]
#[command(name = "spindle", about = "AI-powered file organizer")]
pub struct CliArgs {
  /// Directories to scan
  pub dirs: Vec<PathBuf>,

  /// Path to config file
  #[arg(short, long)]
  pub config: Option<PathBuf>,

  /// Output directory for organized files
  #[arg(short, long)]
  pub output: Option<PathBuf>,

  /// Dry run — show plan without executing
  #[arg(short = 'n', long)]
  pub dry_run: bool,

  /// Skip AI analysis (only hash-based dedup)
  #[arg(long)]
  pub no_ai: bool,

  /// Anthropic API key
  #[arg(long, env = "ANTHROPIC_API_KEY")]
  pub api_key: Option<String>,

  /// Maximum number of files to analyze with AI
  #[arg(long)]
  pub max_files: Option<usize>,

  /// Maximum cost in USD before stopping
  #[arg(long)]
  pub max_cost: Option<f64>,

  /// Anthropic API base URL (for proxies like whetstone)
  #[arg(long, env = "ANTHROPIC_BASE_URL")]
  pub api_base_url: Option<String>,

  /// Verbosity level (-v, -vv, -vvv)
  #[arg(short, long, action = clap::ArgAction::Count)]
  pub verbose: u8,

  /// Only find exact duplicates (no AI, no grouping, no TUI)
  #[arg(long)]
  pub dupes_only: bool,

  /// Include trash/recycle bin folders in scan (excluded by default)
  #[arg(long)]
  pub include_trash: bool,

  /// Undo the last operation (reads undo log from output directory)
  #[arg(long)]
  pub undo: bool,

  /// Path to undo log file (defaults to <output>/.spindle_undo.json)
  #[arg(long)]
  pub undo_log: Option<PathBuf>,

  /// Filter by file type category (image, video, audio, document, archive).
  /// Aliases: photo, movie, music, pdf, zip, etc. Comma-separated or repeated.
  #[arg(
    short = 't',
    long = "type",
    value_delimiter = ',',
    value_parser = parse_file_category,
  )]
  pub file_types: Vec<FileCategory>,
}

fn parse_file_category(s: &str) -> Result<FileCategory, String> {
  s.parse()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
  #[serde(default)]
  pub general: GeneralConfig,
  #[serde(default)]
  pub ai: AiConfig,
  #[serde(default)]
  pub duplicates: DuplicateConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
  #[serde(default = "default_target_dirs")]
  pub target_dirs: Vec<PathBuf>,
  #[serde(default = "default_output_dir")]
  pub output_dir: PathBuf,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AiConfig {
  pub api_key: Option<String>,
  pub base_url: Option<String>,
  #[serde(default = "default_model")]
  pub model: String,
  #[serde(default = "default_max_concurrent")]
  pub max_concurrent_requests: usize,
  #[serde(default = "default_max_files")]
  pub max_files_to_analyze: usize,
  #[serde(default = "default_skip_size")]
  pub skip_files_larger_than_mb: u64,
  #[serde(default = "default_true")]
  pub cache_responses: bool,
  #[serde(default = "default_max_retries")]
  pub max_retries: usize,
}

impl std::fmt::Debug for AiConfig {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("AiConfig")
      .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
      .field("model", &self.model)
      .field("max_concurrent_requests", &self.max_concurrent_requests)
      .field("max_files_to_analyze", &self.max_files_to_analyze)
      .field(
        "skip_files_larger_than_mb",
        &self.skip_files_larger_than_mb,
      )
      .field("cache_responses", &self.cache_responses)
      .field("max_retries", &self.max_retries)
      .finish()
  }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DuplicateConfig {
  #[serde(default = "default_threshold")]
  pub near_duplicate_threshold: u32,
}

impl Default for GeneralConfig {
  fn default() -> Self {
    Self {
      target_dirs: default_target_dirs(),
      output_dir: default_output_dir(),
    }
  }
}

impl Default for AiConfig {
  fn default() -> Self {
    Self {
      api_key: None,
      base_url: None,
      model: default_model(),
      max_concurrent_requests: default_max_concurrent(),
      max_files_to_analyze: default_max_files(),
      skip_files_larger_than_mb: default_skip_size(),
      cache_responses: true,
      max_retries: default_max_retries(),
    }
  }
}

impl Default for DuplicateConfig {
  fn default() -> Self {
    Self {
      near_duplicate_threshold: default_threshold(),
    }
  }
}

fn default_target_dirs() -> Vec<PathBuf> {
  let home = dirs_home();
  vec![home.join("Documents"), home.join("Downloads")]
}

fn default_output_dir() -> PathBuf {
  dirs_home().join("Organized")
}

fn default_model() -> String {
  "claude-opus-4-8".to_string()
}

fn default_max_concurrent() -> usize {
  5
}

fn default_max_files() -> usize {
  500
}

fn default_skip_size() -> u64 {
  100
}

fn default_threshold() -> u32 {
  8
}

fn default_max_retries() -> usize {
  3
}

fn default_true() -> bool {
  true
}

fn dirs_home() -> PathBuf {
  directories::BaseDirs::new()
    .map(|d| d.home_dir().to_path_buf())
    .unwrap_or_else(|| PathBuf::from("~"))
}

fn config_path() -> PathBuf {
  directories::ProjectDirs::from("", "", "spindle")
    .map(|d| d.config_dir().to_path_buf())
    .unwrap_or_else(|| dirs_home().join(".config").join("spindle"))
    .join("config.toml")
}

impl Config {
  pub fn load(cli: &CliArgs) -> Result<Self> {
    let path = cli.config.clone().unwrap_or_else(config_path);

    let mut config = if path.exists() {
      let content = std::fs::read_to_string(&path)
        .context("Failed to read config file")?;
      toml::from_str(&content)
        .context("Failed to parse config file")?
    } else {
      Config {
        general: GeneralConfig::default(),
        ai: AiConfig::default(),
        duplicates: DuplicateConfig::default(),
      }
    };

    if !cli.dirs.is_empty() {
      config.general.target_dirs = cli.dirs.clone();
    }
    if let Some(ref output) = cli.output {
      config.general.output_dir = output.clone();
    }
    if let Some(ref key) = cli.api_key {
      config.ai.api_key = Some(key.clone());
    }
    if let Some(max) = cli.max_files {
      config.ai.max_files_to_analyze = max;
    }
    if let Some(ref url) = cli.api_base_url {
      config.ai.base_url = Some(url.clone());
    }

    Ok(config)
  }

  pub fn api_key(&self) -> Result<&str> {
    self.ai.api_key.as_deref().context(
      "No API key provided. Set ANTHROPIC_API_KEY env var, \
       pass --api-key, or add api_key to config.toml",
    )
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use std::fs;
  use tempfile::TempDir;

  fn empty_cli() -> CliArgs {
    CliArgs {
      dirs: vec![],
      config: None,
      output: None,
      dry_run: false,
      no_ai: false,
      api_key: None,
      max_files: None,
      max_cost: None,
      api_base_url: None,
      verbose: 0,
      include_trash: false,
      dupes_only: false,
      undo: false,
      undo_log: None,
      file_types: vec![],
    }
  }

  #[test]
  fn load_returns_defaults_when_no_config_file() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.ai.model, "claude-opus-4-8");
    assert_eq!(config.ai.max_concurrent_requests, 5);
    assert_eq!(config.duplicates.near_duplicate_threshold, 8);
  }

  #[test]
  fn load_parses_valid_toml() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("config.toml");
    fs::write(
      &toml_path,
      r#"
[ai]
model = "claude-haiku-4-5-20251001"
max_concurrent_requests = 3

[duplicates]
near_duplicate_threshold = 12
"#,
    )
    .unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.ai.model, "claude-haiku-4-5-20251001");
    assert_eq!(config.ai.max_concurrent_requests, 3);
    assert_eq!(config.duplicates.near_duplicate_threshold, 12);
  }

  #[test]
  fn cli_dirs_override_config() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      dirs: vec![PathBuf::from("/custom/dir")],
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(
      config.general.target_dirs,
      vec![PathBuf::from("/custom/dir")]
    );
  }

  #[test]
  fn cli_output_overrides_config() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      output: Some(PathBuf::from("/my/output")),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(
      config.general.output_dir,
      PathBuf::from("/my/output")
    );
  }

  #[test]
  fn cli_api_key_overrides_config() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      api_key: Some("sk-test-key".to_string()),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.api_key().unwrap(), "sk-test-key");
  }

  #[test]
  fn cli_max_files_overrides_config() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      max_files: Some(42),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.ai.max_files_to_analyze, 42);
  }

  #[test]
  fn api_key_returns_error_when_not_set() {
    let config = Config {
      general: GeneralConfig::default(),
      ai: AiConfig::default(),
      duplicates: DuplicateConfig::default(),
    };

    let result = config.api_key();

    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No API key"));
  }

  #[test]
  fn api_key_returns_key_when_set() {
    let mut config = Config {
      general: GeneralConfig::default(),
      ai: AiConfig::default(),
      duplicates: DuplicateConfig::default(),
    };
    config.ai.api_key = Some("sk-my-key".to_string());

    assert_eq!(config.api_key().unwrap(), "sk-my-key");
  }

  #[test]
  fn debug_redacts_api_key() {
    let ai_config = AiConfig {
      api_key: Some("sk-secret-key-12345".to_string()),
      ..Default::default()
    };

    let debug_output = format!("{:?}", ai_config);

    assert!(!debug_output.contains("sk-secret-key-12345"));
    assert!(debug_output.contains("[REDACTED]"));
  }

  #[test]
  fn load_errors_on_invalid_toml() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("bad.toml");
    fs::write(&toml_path, "this is not [valid toml {{{{").unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      ..empty_cli()
    };

    let result = Config::load(&cli);

    assert!(result.is_err());
  }

  #[test]
  fn default_ai_config_has_sane_values() {
    let ai = AiConfig::default();

    assert!(ai.api_key.is_none());
    assert_eq!(ai.max_concurrent_requests, 5);
    assert_eq!(ai.max_files_to_analyze, 500);
    assert_eq!(ai.skip_files_larger_than_mb, 100);
    assert!(ai.cache_responses);
  }

  #[test]
  fn toml_api_key_loaded_from_file() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("config.toml");
    fs::write(
      &toml_path,
      r#"
[ai]
api_key = "sk-from-file"
"#,
    )
    .unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.api_key().unwrap(), "sk-from-file");
  }

  #[test]
  fn cli_api_key_takes_precedence_over_toml() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("config.toml");
    fs::write(
      &toml_path,
      r#"
[ai]
api_key = "sk-from-file"
"#,
    )
    .unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      api_key: Some("sk-from-cli".to_string()),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(config.api_key().unwrap(), "sk-from-cli");
  }

  #[test]
  fn cli_api_base_url_overrides_config() {
    let dir = TempDir::new().unwrap();
    let cli = CliArgs {
      config: Some(dir.path().join("nonexistent.toml")),
      api_base_url: Some("https://proxy.example.com".to_string()),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(
      config.ai.base_url.as_deref(),
      Some("https://proxy.example.com")
    );
  }

  #[test]
  fn toml_base_url_loaded_from_file() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("config.toml");
    fs::write(
      &toml_path,
      r#"
[ai]
base_url = "https://whetstone.local"
"#,
    )
    .unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(
      config.ai.base_url.as_deref(),
      Some("https://whetstone.local")
    );
  }

  #[test]
  fn cli_base_url_takes_precedence_over_toml() {
    let dir = TempDir::new().unwrap();
    let toml_path = dir.path().join("config.toml");
    fs::write(
      &toml_path,
      r#"
[ai]
base_url = "https://from-file.example.com"
"#,
    )
    .unwrap();

    let cli = CliArgs {
      config: Some(toml_path),
      api_base_url: Some("https://from-cli.example.com".to_string()),
      ..empty_cli()
    };

    let config = Config::load(&cli).unwrap();

    assert_eq!(
      config.ai.base_url.as_deref(),
      Some("https://from-cli.example.com")
    );
  }

  #[test]
  fn default_ai_config_has_no_base_url() {
    let ai = AiConfig::default();

    assert!(ai.base_url.is_none());
  }
}
