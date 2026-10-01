use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use hypercarry_core::info::Network;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
pub enum Scope {
    Read,
    Write,
    Trade,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub network: Network,
    pub coins: BTreeSet<String>,
    pub analytics_bin: PathBuf,
    pub analytics_sha256: String,
    pub dataset: PathBuf,
    pub trade_directory: PathBuf,
    #[serde(default)]
    pub captures: BTreeMap<String, PathBuf>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub execution: Option<ExecutionConfig>,
}

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "input must be a regular file"
    );
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(1_048_577)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 1_048_576, "input exceeds 1 MiB");
    serde_json::from_slice(&bytes).context("invalid JSON input")
}

impl Config {
    pub fn load(path: &Path, scopes: &BTreeSet<Scope>) -> Result<Self> {
        let config: Self = read_json(path)?;
        ensure!(config.schema_version == 1, "unsupported MCP configuration");
        ensure!(
            !config.coins.is_empty() && config.coins.len() <= 100,
            "allowlist must contain 1..100 coins"
        );
        for coin in &config.coins {
            ensure!(
                coin.len() <= 64
                    && coin
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_:@".contains(&b)),
                "invalid coin allowlist"
            );
        }
        ensure!(
            (100..=120_000).contains(&config.timeout_ms),
            "timeout must be 100ms..120s"
        );
        crate::process::verify_binary(&config.analytics_bin, &config.analytics_sha256)?;
        for path in [&config.dataset, &config.trade_directory] {
            ensure!(path.is_absolute(), "storage paths must be absolute");
            if !path.exists() && scopes.contains(&Scope::Write) {
                fs::create_dir_all(path)?;
            }
            if path.exists() {
                ensure!(
                    fs::symlink_metadata(path)?.is_dir() && fs::canonicalize(path)? == *path,
                    "storage must be a canonical directory without symlinks"
                );
            }
        }
        for (id, path) in &config.captures {
            validate_id(id)?;
            ensure!(
                path.is_absolute() && fs::symlink_metadata(path)?.file_type().is_file(),
                "capture must be an absolute regular file"
            );
        }
        if scopes.contains(&Scope::Trade) {
            ensure!(
                config.network == Network::Mainnet,
                "trading requires explicit mainnet network"
            );
            let execution = config
                .execution
                .as_ref()
                .context("trade scope requires execution configuration")?;
            crate::process::verify_binary(&execution.executor_bin, &execution.executor_sha256)?;
            ensure!(
                fs::canonicalize(&execution.approval_directory)? == execution.approval_directory,
                "approval directory must be canonical"
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                ensure!(
                    fs::metadata(&execution.approval_directory)?
                        .permissions()
                        .mode()
                        .trailing_zeros()
                        >= 6,
                    "approval directory must be private"
                );
            }
            for path in [
                &execution.config,
                &execution.evidence,
                &execution.approval_directory,
                &execution.executor_bin,
            ] {
                ensure!(path.is_absolute(), "execution paths must be absolute");
                let canonical = fs::canonicalize(path)?;
                ensure!(
                    !canonical.starts_with(&config.dataset)
                        && !canonical.starts_with(&config.trade_directory),
                    "execution authority must be outside MCP-writable storage"
                );
            }
        }
        Ok(config)
    }
    pub fn coin(&self, coin: &str) -> Result<()> {
        ensure!(
            self.coins.contains(coin),
            "coin is outside the server allowlist"
        );
        Ok(())
    }
    pub fn trade_path(&self, id: &str) -> Result<PathBuf> {
        validate_id(id)?;
        Ok(self.trade_directory.join(format!("{id}.json")))
    }
}
pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.is_empty()
            && id.len() <= 64
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "ID must contain 1..64 letters, digits, underscores or hyphens"
    );
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    pub executor_bin: PathBuf,
    pub executor_sha256: String,
    pub config: PathBuf,
    pub evidence: PathBuf,
    pub approval_directory: PathBuf,
}
