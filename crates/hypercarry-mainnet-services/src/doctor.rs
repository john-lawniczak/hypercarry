//! Static operator setup diagnostics. No provider execution, venue calls or writes.
use crate::{digest, policy::ServicePolicy, require_artifact, require_executable};
use anyhow::{Result, ensure};
use hypercarry_executor::config;
use hypercarry_mainnet_config::read_json;
use hypercarry_supervisor::{config::SupervisorConfig, operations::verify_artifact};
use serde::Serialize;
use std::path::Path;

#[derive(Serialize)]
pub struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}
#[derive(Serialize)]
pub struct Report {
    schema_version: u32,
    scope: &'static str,
    pub passed: bool,
    trading_authorized: bool,
    checks: Vec<Check>,
}
impl Report {
    fn record(&mut self, name: &'static str, result: Result<()>) {
        let passed = result.is_ok();
        self.passed &= passed;
        self.checks.push(Check {
            name,
            passed,
            detail: match result {
                Ok(()) => "passed".into(),
                Err(error) => error.to_string(),
            },
        });
    }
}

/// Check setup bindings and installed bytes without needing release approval.
/// The report is never evidence that the account or infrastructure is live-ready.
pub fn check(
    config_path: &Path,
    policy_path: &Path,
    supervisor_path: &Path,
    artifacts: &Path,
) -> Report {
    let mut report = Report {
        schema_version: 1,
        scope: "offline_setup_only",
        passed: true,
        trading_authorized: false,
        checks: Vec::new(),
    };
    let loaded = load(config_path, policy_path, supervisor_path);
    let (config, policy, supervisor) = match loaded {
        Ok(value) => {
            report.record("configuration", Ok(()));
            value
        }
        Err(error) => {
            report.record("configuration", Err(error));
            return report;
        }
    };
    let runtime = &config.runtime;
    report.record(
        "service_policy_binding",
        (|| {
            ensure!(
                runtime.services_policy_digest.as_deref() == Some(&digest(&policy)?),
                "service policy digest mismatch or missing"
            );
            Ok(())
        })(),
    );
    report.record(
        "supervisor_policy_binding",
        (|| {
            ensure!(
                runtime.supervisor_policy_digest.as_deref() == Some(&supervisor.digest()?),
                "supervisor policy digest mismatch or missing"
            );
            Ok(())
        })(),
    );
    report.record("timeout_ordering", (|| {
        ensure!(policy.backend_timeout_ms < runtime.signer_timeout_ms && runtime.signer_timeout_ms < runtime.action_ttl_ms,
            "require backend timeout < signer timeout < signed-action lifetime");
        ensure!(i64::try_from(supervisor.sample_interval_ms)? < config.gate.max_health_age_ms && i64::try_from(supervisor.sample_interval_ms)? < runtime.risk.max_market_data_age_ms,
            "sample interval must leave time for observations inside health and market-data freshness limits"); Ok(())
    })());
    report.record(
        "provider_artifact",
        require_executable(&policy.backend, &policy.backend_sha256),
    );
    if let Some(path) = &policy.backend_config {
        report.record(
            "provider_configuration",
            require_artifact(
                path,
                policy.backend_config_sha256.as_deref().unwrap_or_default(),
            ),
        );
    }
    report.record(
        "signer_artifact",
        require_executable(
            &artifacts.join("hypercarry-mainnet-signer"),
            &policy.signer_sha256,
        ),
    );
    report.record(
        "watchdog_artifact",
        require_executable(
            &artifacts.join("hypercarry-watchdog"),
            &policy.watchdog_sha256,
        ),
    );
    report.record(
        "operational_probe",
        (|| {
            let probe = supervisor
                .operational_probe
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("operational probe is not configured"))?;
            require_executable(&probe.executable, &probe.sha256)?;
            if let Some(path) = &probe.config {
                verify_artifact(path, probe.config_sha256.as_deref().unwrap_or_default())?;
            }
            ensure!(
                i64::try_from(probe.timeout_ms)? < config.gate.max_health_age_ms
                    && i64::try_from(probe.timeout_ms)? < runtime.risk.max_market_data_age_ms,
                "probe timeout exhausts observation freshness budget"
            );
            Ok(())
        })(),
    );
    report
}

fn load(
    config_path: &Path,
    policy_path: &Path,
    supervisor_path: &Path,
) -> Result<(config::Config, ServicePolicy, SupervisorConfig)> {
    let config = config::load(config_path)?;
    let policy: ServicePolicy = read_json(policy_path)?;
    policy.validate(&config.runtime)?;
    let supervisor = SupervisorConfig::load(supervisor_path)?;
    supervisor.validate_runtime(&config.runtime, supervisor_path)?;
    let r = &config.runtime;
    let signer_lock = policy.signer_state.with_extension("lock");
    let watchdog_lock = policy.watchdog_state.with_extension("lock");
    let supervisor_lock = r.health_file.with_extension("supervisor.lock");
    hypercarry_mainnet_config::validate_disjoint_paths(
        [
            config_path,
            policy_path,
            supervisor_path,
            r.signer_socket.as_path(),
            &r.journal,
            &r.health_file,
            &r.heartbeat,
            &r.kill_switch,
            &policy.signer_state,
            &policy.watchdog_state,
            &policy.executor_lease,
            &signer_lock,
            &watchdog_lock,
            &supervisor_lock,
            &policy.backend,
        ]
        .into_iter()
        .chain(policy.backend_config.as_deref())
        .chain(
            supervisor
                .operational_probe
                .as_ref()
                .map(|probe| probe.executable.as_path()),
        )
        .chain(
            supervisor
                .operational_probe
                .as_ref()
                .and_then(|probe| probe.config.as_deref()),
        ),
    )?;
    ensure!(
        supervisor.executor_config == config_path,
        "supervisor names another executor config"
    );
    Ok((config, policy, supervisor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;
    #[test]
    fn offline_setup_passes_without_approval_but_rejects_changed_bindings_and_bytes() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let mut runtime: hypercarry_mainnet_config::RuntimeConfig = serde_json::from_str(
            include_str!("../../../docs/mainnet-runtime-config.example.json"),
        )
        .unwrap();
        runtime.journal = root.join("journal");
        runtime.health_file = root.join("health");
        runtime.signer_socket = root.join("socket");
        runtime.heartbeat = root.join("heartbeat");
        runtime.kill_switch = root.join("STOP");
        let mut policy: ServicePolicy = serde_json::from_str(include_str!(
            "../../../docs/mainnet-services-policy.example.json"
        ))
        .unwrap();
        policy.signer_state = root.join("signer-state.json");
        policy.watchdog_state = root.join("watchdog-state.json");
        policy.executor_lease = root.join("lease");
        policy.backend = root.join("provider");
        for name in [
            "provider",
            "hypercarry-mainnet-signer",
            "hypercarry-watchdog",
            "probe",
        ] {
            fs::write(root.join(name), b"offline fixture bytes, never executed").unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        let hash = crate::artifact_digest(&policy.backend).unwrap();
        policy.backend_sha256.clone_from(&hash);
        policy.signer_sha256.clone_from(&hash);
        policy.watchdog_sha256.clone_from(&hash);
        runtime.executor_lease = Some(policy.executor_lease.clone());
        runtime.services_policy_digest = Some(digest(&policy).unwrap());
        let config_path = root.join("config.json");
        let policy_path = root.join("policy.json");
        let supervisor_path = root.join("supervisor.json");
        let supervisor: SupervisorConfig = serde_json::from_value(json!({"schema_version":1,"executor_config":config_path,
            "rolling_pnl_window_ms":60000,"sample_interval_ms":1000,"operational_probe":{"executable":root.join("probe"),"sha256":hash,"timeout_ms":500}})).unwrap();
        runtime.supervisor_policy_digest = Some(supervisor.digest().unwrap());
        let config = json!({"schema_version":1,"runtime":runtime,"gate":{
            "source_commit":"a".repeat(40),"cargo_lock_digest":"b".repeat(64),"allowed_market":"hyperliquid:BTC",
            "canary_notional_limit":"10","reviewed_max_canary_notional":"10","testnet_key_id":"provider/testnet",
            "config_revision":"fixture-v1","config_review":"fixture/review","max_health_age_ms":5000,
            "max_rest_latency_ms":1000,"max_private_latency_ms":1000,"authorization_ttl_ms":5000,"integration_config_digest":runtime.digest().unwrap()
        }});
        fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();
        fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
        fs::write(&supervisor_path, serde_json::to_vec(&supervisor).unwrap()).unwrap();
        let report = check(&config_path, &policy_path, &supervisor_path, &root);
        assert!(report.passed, "{}", serde_json::to_string(&report).unwrap());
        assert!(!report.trading_authorized);
        let mut aliased = policy.clone();
        aliased.signer_state = root.join("probe");
        fs::write(&policy_path, serde_json::to_vec(&aliased).unwrap()).unwrap();
        let collision = check(&config_path, &policy_path, &supervisor_path, &root);
        assert_eq!(collision.checks[0].name, "configuration");
        assert!(
            !collision.checks[0].passed,
            "probe executable must not also be signer state"
        );
        fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
        assert!(
            !runtime.journal.exists()
                && !runtime.signer_socket.exists()
                && !policy.signer_state.exists()
        );
        policy.backend_timeout_ms = runtime.signer_timeout_ms;
        fs::write(&policy_path, serde_json::to_vec(&policy).unwrap()).unwrap();
        let report = check(&config_path, &policy_path, &supervisor_path, &root);
        assert!(!report.passed);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == "service_policy_binding" && !c.passed)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == "timeout_ordering" && !c.passed)
        );
        fs::write(&policy.backend, "changed").unwrap();
        let report = check(&config_path, &policy_path, &supervisor_path, &root);
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.name == "provider_artifact" && !c.passed)
        );
        assert!(!runtime.journal.exists());
    }
}
