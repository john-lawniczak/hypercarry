use crate::{
    Gate, SigningRequest, digest, now_ms,
    policy::{Purpose, ServicePolicy, validate_request},
    require_artifact, write_private,
};
use alloy::primitives::Signature;
use anyhow::{Context, Result, bail, ensure};
use chrono::DateTime;
use fs2::FileExt;
use hypercarry_execution::{
    ComprehensiveRiskPolicy, ExecutionError, ExecutionNetwork, FileKillSwitch, RiskDecision,
    RiskPolicy, Signer,
};
use hypercarry_executor::{config::Config, safety::SafetySource};
use hypercarry_mainnet_config::{read_json, validate_private_path};
use hypersdk::hypercore::{Action, Chain};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::{
        fs::OpenOptionsExt,
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    process::{Command, Stdio},
    str::FromStr,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::{Duration, Instant},
};

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    policy_digest: String,
    highest_nonce: u64,
    placement_reserved: bool,
}

pub struct Identity<'a>(pub &'a str);
impl Signer for Identity<'_> {
    type Error = ExecutionError;
    fn network(&self) -> ExecutionNetwork {
        ExecutionNetwork::Mainnet
    }
    fn key_id(&self) -> &str {
        self.0
    }
    fn sign(&self, _: &[u8]) -> Result<Vec<u8>, Self::Error> {
        Err(ExecutionError::Policy("identity cannot sign".into()))
    }
}

fn placement_ready(config: &Config, gate: &Gate, evidence_digest: &str) -> Result<()> {
    let r = &config.runtime;
    gate.recheck_submission_with_clock(
        &r.order,
        evidence_digest,
        || {
            i64::try_from(now_ms().map_err(|_| ExecutionError::Policy("clock unavailable".into()))?)
                .map_err(|_| ExecutionError::Policy("clock overflow".into()))
        },
        &Identity(&r.signer_alias),
    )?;
    let risk = ComprehensiveRiskPolicy::new(
        r.risk.clone(),
        SafetySource(r.clone(), config.gate.max_health_age_ms),
        FileKillSwitch::new(&r.kill_switch),
    )?;
    ensure!(
        matches!(risk.evaluate(&r.order)?, RiskDecision::Allow { .. }),
        "placement risk denied"
    );
    Ok(())
}

/// Executes only the hash-pinned provider. Never logs provider stdout/stderr.
/// The provider receives a single JSON prehash request and must return one hex
/// signature then exit. A timeout kills/reaps it and consumes the reserved nonce.
fn backend_signature(
    request: &SigningRequest,
    policy: &ServicePolicy,
    stop: &AtomicBool,
) -> Result<Value> {
    require_artifact(&policy.backend, &policy.backend_sha256)?;
    let action: Action = serde_json::from_value(request.action.clone())?;
    let expires = DateTime::from_timestamp_millis(i64::try_from(request.expires_after)?)
        .context("invalid expiry")?;
    let hash = action.prehash(request.nonce, None, Some(expires), Chain::Mainnet)?;
    let mut child = Command::new(&policy.backend)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("backend unavailable")?;
    let input = json!({"schema_version":1,"network":"mainnet","key_id":request.key_id,"signer_address":request.signer_address,"prehash":hash.to_string()});
    let write_result = (|| -> Result<()> {
        let mut stdin = child.stdin.take().context("backend stdin missing")?;
        serde_json::to_writer(&mut stdin, &input)?;
        stdin.write_all(b"\n")?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        bail!("backend input failed");
    }
    let stdout = child.stdout.take().context("backend stdout missing")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(1025).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if stop.load(Ordering::Relaxed)
            || started.elapsed() > Duration::from_millis(policy.backend_timeout_ms)
        {
            let _ = child.kill();
            let _ = child.wait();
            bail!("backend stopped or timed out; nonce remains consumed");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let bytes = receive
        .recv_timeout(Duration::from_millis(100))
        .context("backend output did not close after process exit")??;
    ensure!(
        status.success() && bytes.len() <= 1024,
        "backend refused or returned oversized output"
    );
    let text = std::str::from_utf8(&bytes)
        .context("invalid backend output")?
        .trim();
    let signature =
        Signature::from_str(text).map_err(|_| anyhow::anyhow!("invalid backend signature"))?;
    ensure!(
        signature
            .recover_address_from_prehash(&hash)?
            .to_string()
            .to_ascii_lowercase()
            == request.signer_address,
        "signature identity mismatch"
    );
    // Serialize the SDK action, which defines the ordered signing wire format.
    Ok(
        json!({"action":action,"nonce":request.nonce,"expiresAfter":request.expires_after,"vaultAddress":null,
        "signature":{"r":format!("0x{:064x}",signature.r()),"s":format!("0x{:064x}",signature.s()),"v":signature.v_byte()}}),
    )
}

fn reserve(state: &mut State, request: &SigningRequest, purpose: Purpose) -> Result<()> {
    ensure!(
        request.nonce > state.highest_nonce,
        "nonce replay or regression"
    );
    ensure!(
        purpose != Purpose::Placement || !state.placement_reserved,
        "canary signing capability already consumed"
    );
    state.highest_nonce = request.nonce;
    state.placement_reserved |= purpose == Purpose::Placement;
    Ok(())
}

/// The immutable, reviewed context every request is answered against. Held once
/// per process so a request cannot be served under a different binding than the
/// one `serve` verified, and so the venue client is not rebuilt per check.
struct Service<'a> {
    client: reqwest::blocking::Client,
    config: &'a Config,
    policy: &'a ServicePolicy,
    gate: &'a Gate,
    evidence_digest: &'a str,
}

fn respond(
    stream: &mut UnixStream,
    service: &Service<'_>,
    state: &mut State,
    stop: &AtomicBool,
) -> Result<()> {
    let Service {
        client,
        config,
        policy,
        gate,
        evidence_digest,
    } = service;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let mut bytes = Vec::new();
    (&mut *stream).take(65_537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "oversized request");
    let request: SigningRequest = serde_json::from_slice(&bytes)?;
    let purpose = validate_request(&request, &config.runtime, policy, now_ms()?)?;
    if purpose == Purpose::Placement {
        placement_ready(config, gate, evidence_digest)?;
    }
    crate::check_agent(client, &config.runtime)?;
    reserve(state, &request, purpose)?;
    write_private(&policy.signer_state, state)?;
    let signed = backend_signature(&request, policy, stop)?;
    validate_request(&request, &config.runtime, policy, now_ms()?)?;
    if purpose == Purpose::Placement {
        placement_ready(config, gate, evidence_digest)?;
    }
    crate::check_agent(client, &config.runtime)?;
    validate_request(&request, &config.runtime, policy, now_ms()?)?;
    ensure!(!stop.load(Ordering::Relaxed), "signer stopping");
    serde_json::to_writer(
        stream,
        &json!({"schema_version":1,"network":"mainnet","key_id":request.key_id,"signer_address":request.signer_address,"signed_request":signed}),
    )?;
    Ok(())
}

/// # Errors
/// Rejects unreviewed artifacts, unsafe state, concurrent service instances or socket failures.
pub fn serve(
    config: &Config,
    policy: &ServicePolicy,
    gate: &Gate,
    evidence_digest: &str,
    stop: &AtomicBool,
) -> Result<()> {
    require_artifact(&std::env::current_exe()?, &policy.signer_sha256)?;
    let binding = digest(&(config.runtime.digest()?, policy, evidence_digest))?;
    let lock_path = policy.signer_state.with_extension("lock");
    validate_private_path(&lock_path)?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&lock_path)?;
    lock.try_lock_exclusive()
        .context("signer already running")?;
    let mut state: State = if policy.signer_state.exists() {
        read_json(&policy.signer_state)?
    } else {
        State {
            policy_digest: binding.clone(),
            ..State::default()
        }
    };
    ensure!(
        state.policy_digest == binding,
        "signer state belongs to another review; retain it and use a new reviewed state path"
    );
    write_private(&policy.signer_state, &state)?;
    let socket = &config.runtime.signer_socket;
    validate_private_path(socket)?;
    ensure!(
        !socket.exists(),
        "socket already exists; inspect stale service before removing it"
    );
    let listener = UnixListener::bind(socket)?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let service = Service {
        client: crate::venue_client()?,
        config,
        policy,
        gate,
        evidence_digest,
    };
    let outcome = (|| -> Result<()> {
        while !stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    if respond(&mut stream, &service, &mut state, stop).is_err() {
                        // No backend output, request or signature appears in logs.
                        eprintln!(
                            "signer: request refused; inspect configuration, readiness and retained nonce state"
                        );
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    })();
    // Socket cleanup must never replace the reason the loop stopped: that reason
    // is what an operator reconciles the retained nonce state against.
    let removed = fs::remove_file(socket).map_err(anyhow::Error::from);
    drop(lock);
    outcome.and(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_survives_failure_and_refuses_replay_or_second_canary() {
        let mut state = State::default();
        let mut request = SigningRequest {
            schema_version: 1,
            network: ExecutionNetwork::Mainnet,
            key_id: "mainnet".into(),
            signer_address: String::new(),
            nonce: 10,
            expires_after: 20,
            action: json!({}),
        };
        reserve(&mut state, &request, Purpose::Placement).unwrap();
        assert!(reserve(&mut state, &request, Purpose::Cancel).is_err());
        request.nonce = 11;
        assert!(reserve(&mut state, &request, Purpose::Placement).is_err());
        reserve(&mut state, &request, Purpose::Cancel).unwrap();
        let recovered: State =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert!(recovered.placement_reserved);
        assert_eq!(recovered.highest_nonce, 11);
    }
    #[test]
    fn artifact_hash_is_real_sha256() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            crate::artifact_digest(temp.path()).unwrap(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}

#[cfg(test)]
mod backend_tests {
    use super::*;
    use alloy::signers::{SignerSync, local::PrivateKeySigner};
    use std::os::unix::fs::PermissionsExt;

    fn fixture(script: Option<&str>) -> (tempfile::TempDir, SigningRequest, ServicePolicy) {
        // Public deterministic fixture key, never a credential or production key.
        let signer: PrivateKeySigner =
            "0000000000000000000000000000000000000000000000000000000000000001"
                .parse()
                .unwrap();
        let runtime: hypercarry_mainnet_config::RuntimeConfig = serde_json::from_str(include_str!(
            "../../../docs/mainnet-runtime-config.example.json"
        ))
        .unwrap();
        let nonce = now_ms().unwrap();
        let request = SigningRequest {
            schema_version: 1,
            network: ExecutionNetwork::Mainnet,
            key_id: "fixture/mainnet".into(),
            signer_address: signer.address().to_string().to_ascii_lowercase(),
            nonce,
            expires_after: nonce + 10_000,
            action: crate::policy::exact_order(&runtime).unwrap(),
        };
        let action: Action = serde_json::from_value(request.action.clone()).unwrap();
        let hash = action
            .prehash(
                nonce,
                None,
                Some(
                    DateTime::from_timestamp_millis(i64::try_from(request.expires_after).unwrap())
                        .unwrap(),
                ),
                Chain::Mainnet,
            )
            .unwrap();
        let signature = signer.sign_hash_sync(&hash).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let backend = temp.path().join("provider");
        let default = format!("#!/bin/sh\nprintf '%s\\n' '{signature}'\n");
        fs::write(&backend, script.unwrap_or(&default)).unwrap();
        fs::set_permissions(&backend, fs::Permissions::from_mode(0o700)).unwrap();
        let mut policy: ServicePolicy = serde_json::from_str(include_str!(
            "../../../docs/mainnet-services-policy.example.json"
        ))
        .unwrap();
        policy.backend = backend;
        policy.backend_sha256 = crate::artifact_digest(&policy.backend).unwrap();
        (temp, request, policy)
    }
    #[test]
    fn backend_mainnet_signature_is_recovered_and_wrong_identity_is_refused() {
        let (_temp, mut request, policy) = fixture(None);
        let signed = backend_signature(&request, &policy, &AtomicBool::new(false)).unwrap();
        assert_eq!(signed["nonce"], request.nonce);
        assert_eq!(
            signed["action"],
            serde_json::to_value(serde_json::from_value::<Action>(request.action.clone()).unwrap())
                .unwrap()
        );
        request.signer_address = "0x2222222222222222222222222222222222222222".into();
        assert!(backend_signature(&request, &policy, &AtomicBool::new(false)).is_err());
    }
    #[test]
    fn backend_timeout_is_bounded_and_invalid_output_cannot_escape() {
        let (_temp, request, mut policy) = fixture(Some("#!/bin/sh\nexec /bin/sleep 10\n"));
        policy.backend_timeout_ms = 100;
        let started = Instant::now();
        assert!(backend_signature(&request, &policy, &AtomicBool::new(false)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let (_temp, request, policy) = fixture(Some("#!/bin/sh\nprintf 'not-a-signature'\n"));
        assert!(backend_signature(&request, &policy, &AtomicBool::new(false)).is_err());
    }
}
