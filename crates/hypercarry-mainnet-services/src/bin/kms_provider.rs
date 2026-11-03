//! Noninteractive AWS KMS adapter. Credentials come only from EC2 workload
//! identity; the parent signer authenticates and restricts the requested action.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use clap::Parser;
use hypercarry_mainnet_config::read_json;
use hypercarry_mainnet_services::require_executable;
use hypercarry_signing::SignatureRecovery as _;
use hypercarry_signing::{Address, B256, Signature, U256};
use serde::Deserialize;
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

#[derive(Parser)]
struct Args {
    #[arg(
        long,
        required_unless_present = "public_key_json",
        conflicts_with = "public_key_json"
    )]
    config: Option<PathBuf>,
    /// Derive the agent address offline from an AWS `GetPublicKey` JSON response.
    #[arg(long)]
    public_key_json: Option<PathBuf>,
    /// Validate the pinned local CLI/configuration without stdin or any AWS call.
    #[arg(long, conflicts_with_all = ["public_key_json", "check_access"], requires = "config")]
    check_config: bool,
    /// Query KMS public-key access and verify the pinned identity; never signs.
    #[arg(long, conflicts_with = "public_key_json", requires = "config")]
    check_access: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    schema_version: u32,
    aws_bin: PathBuf,
    aws_sha256: String,
    region: String,
    kms_key_arn: String,
    key_id: String,
    signer_address: Address,
    timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    schema_version: u32,
    network: String,
    key_id: String,
    signer_address: Address,
    prehash: B256,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if let Some(path) = args.public_key_json {
        let public: serde_json::Value = read_json(&path)?;
        println!(
            "{}",
            public_address(&public)?.to_string().to_ascii_lowercase()
        );
        return Ok(());
    }
    let config: Config = read_json(&args.config.context("missing provider config")?)?;
    validate_config(&config)?;
    if args.check_config {
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"static_config":"passed","kms_access":"not_checked","signing_performed":false})
        );
        return Ok(());
    }
    if args.check_access {
        let elapsed = check_access(&config)?;
        println!(
            "{}",
            serde_json::json!({"schema_version":1,"kms_public_key_access":"passed","kms_key_arn":config.kms_key_arn,"signer_address":config.signer_address,"elapsed_ms":elapsed,"signing_performed":false,"sign_permission":"not_checked","venue_agent_authorization":"not_checked"})
        );
        return Ok(());
    }
    let mut input = Vec::new();
    std::io::stdin().take(4097).read_to_end(&mut input)?;
    ensure!(input.len() <= 4096, "provider input exceeds bound");
    let request: Request = serde_json::from_slice(&input)?;
    ensure!(
        request.schema_version == 1
            && request.network == "mainnet"
            && request.key_id == config.key_id
            && request.signer_address == config.signer_address,
        "provider identity mismatch"
    );
    let signature = sign(&config, &request)?;
    println!("{signature}");
    Ok(())
}

fn validate_config(config: &Config) -> Result<()> {
    ensure!(
        config.schema_version == 1 && (100..=10_000).contains(&config.timeout_ms),
        "invalid KMS provider configuration"
    );
    let fields: Vec<_> = config.kms_key_arn.split(':').collect();
    ensure!(
        fields.len() == 6
            && fields[0] == "arn"
            && matches!(fields[1], "aws" | "aws-us-gov" | "aws-cn")
            && fields[2] == "kms"
            && fields[3] == config.region
            && !config.region.is_empty()
            && fields[4].len() == 12
            && fields[4].bytes().all(|b| b.is_ascii_digit())
            && fields[5].starts_with("key/")
            && fields[5].len() > 4,
        "an immutable KMS key ARN matching the configured region is required"
    );
    require_executable(&config.aws_bin, &config.aws_sha256)?;
    ensure!(
        !config.key_id.trim().is_empty() && config.signer_address != Address::ZERO,
        "provider identity is incomplete"
    );
    Ok(())
}

const PUBLIC_KEY_PREFIX: &[u8] = &[
    0x30, 0x56, 0x30, 0x10, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x05, 0x2b,
    0x81, 0x04, 0x00, 0x0a, 0x03, 0x42, 0x00, 0x04,
];

fn public_address(public: &serde_json::Value) -> Result<Address> {
    ensure!(
        public["KeySpec"] == "ECC_SECG_P256K1" && public["KeyUsage"] == "SIGN_VERIFY",
        "KMS key must be secp256k1 SIGN_VERIFY"
    );
    let der = STANDARD.decode(public["PublicKey"].as_str().context("missing public key")?)?;
    // Exact SubjectPublicKeyInfo prefix: id-ecPublicKey, secp256k1, 65-byte
    // uncompressed EC point. A different curve or encoding must be refused.
    ensure!(
        der.len() == 88 && der.starts_with(PUBLIC_KEY_PREFIX),
        "unsupported KMS public key encoding"
    );
    // Prefix/length alone do not establish that the coordinates are on-curve.
    hypercarry_signing::address_from_sec1(&der[PUBLIC_KEY_PREFIX.len() - 1..])
}

fn check_access(config: &Config) -> Result<u128> {
    let start = Instant::now();
    // This standalone diagnostic owns its group. Normal signing deliberately
    // inherits the outer provider's group so the signer's timeout reaches AWS.
    let mut helper = hypercarry_supervisor::process::HelperProcess::spawn(
        Command::new(&config.aws_bin)
            .args([
                "kms",
                "get-public-key",
                "--region",
                &config.region,
                "--key-id",
                &config.kms_key_arn,
                "--output",
                "json",
                "--no-cli-pager",
                "--cli-connect-timeout",
                "1",
                "--cli-read-timeout",
                "2",
            ])
            .env_clear()
            .env("AWS_CONFIG_FILE", "/dev/null")
            .env("AWS_SHARED_CREDENTIALS_FILE", "/dev/null")
            .env("AWS_EC2_METADATA_V1_DISABLED", "true")
            .env("AWS_MAX_ATTEMPTS", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null()),
    )?;
    let stdout = helper.child.stdout.take().context("missing KMS output")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(8193).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    loop {
        ensure!(
            start.elapsed() < Duration::from_millis(config.timeout_ms),
            "KMS access check timed out"
        );
        if let Some(status) = helper.child.try_wait()? {
            ensure!(
                status.success(),
                "KMS public-key access denied or unavailable"
            );
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let bytes = receive
        .recv_timeout(Duration::from_millis(100))
        .context("KMS output did not close")??;
    ensure!(bytes.len() <= 8192, "KMS output exceeds bound");
    let response: serde_json::Value = serde_json::from_slice(&bytes)?;
    ensure!(
        response["KeyId"] == config.kms_key_arn,
        "KMS key ARN mismatch"
    );
    ensure!(
        response["SigningAlgorithms"]
            .as_array()
            .is_some_and(|algorithms| algorithms.iter().any(|a| a == "ECDSA_SHA_256")),
        "KMS signing algorithm unavailable"
    );
    ensure!(
        public_address(&response)? == config.signer_address,
        "KMS public identity mismatch"
    );
    Ok(start.elapsed().as_millis())
}

fn sign(config: &Config, request: &Request) -> Result<Signature> {
    let mut child = Command::new(&config.aws_bin)
        .args([
            "kms",
            "sign",
            "--region",
            &config.region,
            "--key-id",
            &config.kms_key_arn,
            "--message",
            &STANDARD.encode(request.prehash),
            "--message-type",
            "DIGEST",
            "--signing-algorithm",
            "ECDSA_SHA_256",
            "--cli-binary-format",
            "base64",
            "--output",
            "json",
            "--no-cli-pager",
            "--cli-connect-timeout",
            "1",
            "--cli-read-timeout",
            "2",
        ])
        .env_clear()
        .env("AWS_CONFIG_FILE", "/dev/null")
        .env("AWS_SHARED_CREDENTIALS_FILE", "/dev/null")
        .env("AWS_EC2_METADATA_V1_DISABLED", "true")
        .env("AWS_MAX_ATTEMPTS", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("KMS CLI unavailable")?;
    let stdout = child.stdout.take().context("missing KMS output")?;
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.take(8193).read_to_end(&mut bytes).map(|_| bytes);
        let _ = send.send(result);
    });
    let start = Instant::now();
    let outcome = (|| -> Result<Signature> {
        loop {
            ensure!(
                start.elapsed() < Duration::from_millis(config.timeout_ms),
                "KMS timed out"
            );
            if let Some(status) = child.try_wait()? {
                ensure!(status.success(), "KMS refused signing");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let bytes = receive
            .recv_timeout(Duration::from_millis(100))
            .context("KMS output did not close")??;
        ensure!(bytes.len() <= 8192, "KMS output exceeds bound");
        let output: serde_json::Value = serde_json::from_slice(&bytes)?;
        ensure!(
            output["KeyId"] == config.kms_key_arn && output["SigningAlgorithm"] == "ECDSA_SHA_256",
            "KMS response identity mismatch"
        );
        let der = STANDARD.decode(
            output["Signature"]
                .as_str()
                .context("missing KMS signature")?,
        )?;
        recover_der(&der, &request.prehash, config.signer_address)
    })();
    if outcome.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    outcome
}

// KMS returns DER ECDSA without a recovery ID. Require canonical, positive,
// in-range scalars, normalize S, and recover both parities against the pinned
// public address. We never hash the supplied prehash a second time.
fn recover_der(der: &[u8], prehash: &B256, address: Address) -> Result<Signature> {
    ensure!(
        (8..=72).contains(&der.len()) && der[0] == 0x30 && usize::from(der[1]) == der.len() - 2,
        "invalid DER signature"
    );
    let mut rest = &der[2..];
    let r = scalar(&mut rest)?;
    let s = scalar(&mut rest)?;
    ensure!(rest.is_empty(), "trailing DER data");
    let order = U256::from_str_radix(
        "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
        16,
    )?;
    ensure!(
        r > U256::ZERO && r < order && s > U256::ZERO && s < order,
        "signature scalar out of range"
    );
    let normalized = Signature::new(r, s, false).normalized_s();
    for parity in [false, true] {
        let candidate = normalized.with_parity(parity);
        if candidate
            .recover_address_from_prehash(prehash)
            .is_ok_and(|found| found == address)
        {
            return Ok(candidate);
        }
    }
    anyhow::bail!("KMS signature does not recover the pinned agent")
}

fn scalar(rest: &mut &[u8]) -> Result<U256> {
    ensure!(rest.len() >= 3 && rest[0] == 2, "invalid DER integer");
    let len = usize::from(rest[1]);
    ensure!(
        (1..=33).contains(&len) && rest.len() >= len + 2,
        "invalid DER integer length"
    );
    let bytes = &rest[2..len + 2];
    ensure!(
        bytes[0] & 0x80 == 0 && (len == 1 || bytes[0] != 0 || bytes[1] & 0x80 != 0),
        "noncanonical DER integer"
    );
    ensure!(len <= 32 || bytes[0] == 0, "oversized DER integer");
    let value = U256::from_be_slice(if len == 33 { &bytes[1..] } else { bytes });
    *rest = &rest[len + 2..];
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hypercarry_signing::{B256, DigestSigner, SigningKey};
    #[test]
    fn public_key_helper_rejects_off_curve_coordinates() {
        let mut secret = [0; 32];
        secret[31] = 1; // Public fixture.
        let signer = SigningKey::from_slice(&secret).unwrap();
        let mut der = PUBLIC_KEY_PREFIX[..PUBLIC_KEY_PREFIX.len() - 1].to_vec();
        der.extend(signer.verifying_key().to_sec1_point(false).as_bytes());
        let mut response = serde_json::json!({"KeySpec":"ECC_SECG_P256K1","KeyUsage":"SIGN_VERIFY","PublicKey":STANDARD.encode(&der)});
        assert_eq!(public_address(&response).unwrap(), signer.address());
        der[PUBLIC_KEY_PREFIX.len()..].fill(0);
        response["PublicKey"] = STANDARD.encode(&der).into();
        assert!(public_address(&response).is_err());
        der[PUBLIC_KEY_PREFIX.len()..].fill(0xff);
        response["PublicKey"] = STANDARD.encode(&der).into();
        assert!(public_address(&response).is_err());
    }

    #[test]
    fn access_check_is_read_only_and_refuses_wrong_identity_denial_and_timeout() {
        use std::os::unix::fs::PermissionsExt;
        let mut secret = [0; 32];
        secret[31] = 1;
        let signer = SigningKey::from_slice(&secret).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let cli = temp.path().join("aws");
        let mut der = PUBLIC_KEY_PREFIX[..PUBLIC_KEY_PREFIX.len() - 1].to_vec();
        der.extend(signer.verifying_key().to_sec1_point(false).as_bytes());
        let arn = "arn:aws:kms:us-east-1:123456789012:key/fixture";
        let response = serde_json::json!({"KeyId":arn,"KeySpec":"ECC_SECG_P256K1","KeyUsage":"SIGN_VERIFY","SigningAlgorithms":["ECDSA_SHA_256"],"PublicKey":STANDARD.encode(der)});
        let mut config = Config {
            schema_version: 1,
            aws_bin: cli.clone(),
            aws_sha256: String::new(),
            region: "us-east-1".into(),
            kms_key_arn: arn.into(),
            key_id: "fixture".into(),
            signer_address: signer.address(),
            timeout_ms: 1000,
        };
        std::fs::write(&cli, format!("#!/bin/sh\n[ \"$1 $2\" = 'kms get-public-key' ] || exit 42\nprintf '%s' '{response}'\n")).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(check_access(&config).is_ok());
        config.signer_address = Address::ZERO;
        assert!(check_access(&config).is_err());
        std::fs::write(&cli, "#!/bin/sh\nexit 1\n").unwrap();
        assert!(check_access(&config).is_err());
        std::fs::write(&cli, "#!/bin/sh\nexec /bin/sleep 10\n").unwrap();
        config.timeout_ms = 100;
        assert!(check_access(&config).is_err());
    }
    fn encode(signature: Signature) -> Vec<u8> {
        let mut body = Vec::new();
        for scalar in [signature.r(), signature.s()] {
            let bytes = scalar.to_be_bytes::<32>();
            let first = bytes.iter().position(|b| *b != 0).unwrap_or(31);
            let bytes = &bytes[first..];
            let pad = usize::from(bytes[0] & 0x80 != 0);
            body.extend_from_slice(&[2, u8::try_from(bytes.len() + pad).unwrap()]);
            if pad != 0 {
                body.push(0);
            }
            body.extend_from_slice(bytes);
        }
        let mut der = vec![0x30, u8::try_from(body.len()).unwrap()];
        der.extend(body);
        der
    }
    #[test]
    fn kms_der_recovers_exact_digest_and_normalizes_high_s() {
        // Public fixture key; never used for account operations.
        let signer = SigningKey::from_slice(
            &"0000000000000000000000000000000000000000000000000000000000000001"
                .parse::<B256>()
                .unwrap()[..],
        )
        .unwrap();
        let hash = B256::repeat_byte(42);
        let signature = signer.sign_hash_sync(&hash).unwrap().normalized_s();
        let der = encode(signature);
        assert_eq!(
            recover_der(&der, &hash, signer.address()).unwrap(),
            signature
        );
        assert!(recover_der(&der, &B256::ZERO, signer.address()).is_err());
        assert!(recover_der(&der, &hash, Address::ZERO).is_err());
        let order = U256::from_str_radix(
            "fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141",
            16,
        )
        .unwrap();
        let high = Signature::new(signature.r(), order - signature.s(), false);
        assert_eq!(
            recover_der(&encode(high), &hash, signer.address()).unwrap(),
            signature
        );
        for invalid in [
            vec![],
            vec![0x30, 6, 2, 1, 0, 2, 1, 1],
            vec![0x30, 6, 2, 1, 0xff, 2, 1, 1],
        ] {
            assert!(recover_der(&invalid, &hash, signer.address()).is_err());
        }
    }

    #[test]
    fn kms_cli_receives_digest_mode_and_response_identity_is_enforced() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let signer = SigningKey::from_slice(
            &"0000000000000000000000000000000000000000000000000000000000000001"
                .parse::<B256>()
                .unwrap()[..],
        )
        .unwrap();
        let request = Request {
            schema_version: 1,
            network: "mainnet".into(),
            key_id: "fixture".into(),
            signer_address: signer.address(),
            prehash: B256::repeat_byte(42),
        };
        let aws_bin = temp.path().join("aws");
        let arn = "arn:aws:kms:us-east-1:123456789012:key/fixture";
        let response = serde_json::json!({"KeyId":arn,"SigningAlgorithm":"ECDSA_SHA_256","Signature":STANDARD.encode(encode(signer.sign_hash_sync(&request.prehash).unwrap()))});
        let encoded = STANDARD.encode(request.prehash);
        let script = format!(
            "#!/bin/sh\n[ \"$1 $2\" = 'kms sign' ] || exit 1\n[ \"${{10}}\" = 'DIGEST' ] || exit 2\n[ \"${{12}}\" = 'ECDSA_SHA_256' ] || exit 3\n[ \"${{8}}\" = '{encoded}' ] || exit 4\nprintf '%s' '{response}'\n"
        );
        std::fs::write(&aws_bin, script).unwrap();
        std::fs::set_permissions(&aws_bin, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut config = Config {
            schema_version: 1,
            aws_sha256: hypercarry_mainnet_services::artifact_digest(&aws_bin).unwrap(),
            aws_bin,
            region: "us-east-1".into(),
            kms_key_arn: arn.into(),
            key_id: "fixture".into(),
            signer_address: signer.address(),
            timeout_ms: 1000,
        };
        assert!(sign(&config, &request).is_ok());
        config.kms_key_arn = "arn:aws:kms:us-east-1:123456789012:key/other".into();
        assert!(sign(&config, &request).is_err());
    }
}
