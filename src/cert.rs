use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use rustls::{
    pki_types::{
        CertificateDer, PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer,
    },
    ClientConfig, RootCertStore, ServerConfig,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AlpnProtocol {
    Http11,
}

impl AlpnProtocol {
    fn wire_name(self) -> Vec<u8> {
        match self {
            Self::Http11 => b"http/1.1".to_vec(),
        }
    }
}

#[derive(Debug)]
pub struct CertificateAuthority {
    pub cert_path: PathBuf,
    key_path: PathBuf,
}

#[cfg(not(test))]
pub fn install_default_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn ensure_ca(ca_dir: &Path) -> Result<CertificateAuthority> {
    fs::create_dir_all(ca_dir)?;
    let cert_path = ca_dir.join("litellm-relay-ca.pem");
    let key_path = ca_dir.join("litellm-relay-ca-key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok(CertificateAuthority {
            cert_path,
            key_path,
        });
    }
    run_quiet(
        Command::new("openssl")
            .arg("req")
            .arg("-x509")
            .arg("-newkey")
            .arg("rsa:2048")
            .arg("-nodes")
            .arg("-sha256")
            .arg("-days")
            .arg("825")
            .arg("-keyout")
            .arg(&key_path)
            .arg("-out")
            .arg(&cert_path)
            .arg("-subj")
            .arg("/CN=LiteLLM Relay Local Root CA")
            .arg("-addext")
            .arg("basicConstraints=critical,CA:TRUE,pathlen:0")
            .arg("-addext")
            .arg("keyUsage=critical,keyCertSign,cRLSign"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(CertificateAuthority {
        cert_path,
        key_path,
    })
}

pub trait CaTrust: Send + Sync {
    fn trusts(&self, cert_path: &Path) -> bool;
}

pub struct UserCaTrust;

impl CaTrust for UserCaTrust {
    #[cfg(target_os = "macos")]
    fn trusts(&self, cert_path: &Path) -> bool {
        let outcome = run_quiet_within(
            Command::new("/usr/bin/security")
                .arg("verify-cert")
                .arg("-c")
                .arg(cert_path)
                .arg("-l")
                .arg("-L")
                .arg("-q"),
            VERIFY_CERT_TIMEOUT,
        );
        match outcome {
            CommandOutcome::Succeeded => true,
            CommandOutcome::Failed => false,
            CommandOutcome::TimedOut => {
                eprintln!(
                    "cert: `security verify-cert` gave no answer in {} s; counting the Relay CA as untrusted",
                    VERIFY_CERT_TIMEOUT.as_secs()
                );
                false
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn trusts(&self, _cert_path: &Path) -> bool {
        true
    }
}

pub fn trust_command(cert_path: &Path) -> String {
    format!(
        "security add-trusted-cert -r trustRoot -k ~/Library/Keychains/login.keychain-db \"{}\"",
        cert_path.display()
    )
}

pub fn server_tls_config(host: &str, ca_dir: &Path) -> Result<ServerConfig> {
    let (cert_path, key_path) = ensure_leaf_cert(host, ca_dir)?;
    let cert_file = fs::read(&cert_path)?;
    let key_file = fs::read(&key_path)?;
    let certs = load_cert_chain(&cert_file)?;
    let key = load_private_key(&key_file)?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
    config.alpn_protocols = vec![AlpnProtocol::Http11.wire_name()];
    Ok(config)
}

pub fn client_tls_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let mut config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![AlpnProtocol::Http11.wire_name()];
    config
}

fn ensure_leaf_cert(host: &str, ca_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let ca = ensure_ca(ca_dir)?;
    let certs_dir = ca_dir.join("certs");
    fs::create_dir_all(&certs_dir)?;
    let safe_host = safe_cert_name(host);
    let cert_path = certs_dir.join(format!("{safe_host}.pem"));
    let key_path = certs_dir.join(format!("{safe_host}-key.pem"));
    let csr_path = certs_dir.join(format!("{safe_host}.csr"));
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }
    let ext_path = certs_dir.join(format!("{safe_host}.ext"));
    fs::write(
        &ext_path,
        format!(
            "basicConstraints=CA:FALSE\nkeyUsage=digitalSignature,keyEncipherment\nextendedKeyUsage=serverAuth\nsubjectAltName=DNS:{host}\n"
        ),
    )?;
    let req_result = run_quiet(
        Command::new("openssl")
            .arg("req")
            .arg("-newkey")
            .arg("rsa:2048")
            .arg("-nodes")
            .arg("-keyout")
            .arg(&key_path)
            .arg("-out")
            .arg(&csr_path)
            .arg("-subj")
            .arg(format!("/CN={host}")),
    );
    if let Err(error) = req_result {
        let _ = fs::remove_file(&csr_path);
        let _ = fs::remove_file(&ext_path);
        return Err(error);
    }
    let sign_result = run_quiet(
        Command::new("openssl")
            .arg("x509")
            .arg("-req")
            .arg("-in")
            .arg(&csr_path)
            .arg("-CA")
            .arg(&ca.cert_path)
            .arg("-CAkey")
            .arg(&ca.key_path)
            .arg("-CAcreateserial")
            .arg("-out")
            .arg(&cert_path)
            .arg("-days")
            .arg("90")
            .arg("-sha256")
            .arg("-extfile")
            .arg(&ext_path),
    );
    let _ = fs::remove_file(&csr_path);
    let _ = fs::remove_file(&ext_path);
    sign_result?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok((cert_path, key_path))
}

#[cfg(target_os = "macos")]
const VERIFY_CERT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum CommandOutcome {
    Succeeded,
    Failed,
    TimedOut,
}

#[cfg(any(target_os = "macos", test))]
fn run_quiet_within(command: &mut Command, limit: std::time::Duration) -> CommandOutcome {
    let spawned = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = spawned else {
        return CommandOutcome::Failed;
    };
    let deadline = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return CommandOutcome::Succeeded,
            Ok(Some(_)) | Err(_) => return CommandOutcome::Failed,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return CommandOutcome::TimedOut;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
}

fn run_quiet(command: &mut Command) -> Result<()> {
    let status = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("command failed with status {status}"))
    }
}

fn load_cert_chain(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let certs = decode_pem_blocks(pem, "CERTIFICATE")?;
    if certs.is_empty() {
        return Err(anyhow!("leaf certificate not found"));
    }
    Ok(certs.into_iter().map(CertificateDer::from).collect())
}

fn load_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
    if let Some(key) = decode_first_pem_block(pem, "PRIVATE KEY")? {
        return Ok(PrivateKeyDer::from(PrivatePkcs8KeyDer::from(key)));
    }
    if let Some(key) = decode_first_pem_block(pem, "RSA PRIVATE KEY")? {
        return Ok(PrivateKeyDer::from(PrivatePkcs1KeyDer::from(key)));
    }
    if let Some(key) = decode_first_pem_block(pem, "EC PRIVATE KEY")? {
        return Ok(PrivateKeyDer::from(PrivateSec1KeyDer::from(key)));
    }
    Err(anyhow!("leaf private key not found"))
}

fn decode_first_pem_block(pem: &[u8], label: &str) -> Result<Option<Vec<u8>>> {
    Ok(decode_pem_blocks(pem, label)?.into_iter().next())
}

fn decode_pem_blocks(pem: &[u8], label: &str) -> Result<Vec<Vec<u8>>> {
    let text = std::str::from_utf8(pem)?;
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut rest = text;
    let mut blocks = Vec::new();

    while let Some(begin_index) = rest.find(&begin) {
        let block_start = begin_index + begin.len();
        let after_begin = &rest[block_start..];
        let end_index = after_begin
            .find(&end)
            .ok_or_else(|| anyhow!("unterminated PEM block: {label}"))?;
        let encoded: String = after_begin[..end_index]
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        blocks.push(STANDARD.decode(encoded)?);
        rest = &after_begin[end_index + end.len()..];
    }

    Ok(blocks)
}

fn safe_cert_name(host: &str) -> String {
    let cleaned = host
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches(['.', '_'])
        .to_string();
    if cleaned.is_empty() {
        "host".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_cert_name_removes_path_characters() {
        assert_eq!(safe_cert_name("www.notion.so:443"), "www.notion.so_443");
    }

    #[cfg(unix)]
    #[test]
    fn should_give_up_on_a_trust_check_that_never_answers_and_keep_the_answers_of_one_that_does() {
        let started = std::time::Instant::now();
        let hung = run_quiet_within(
            Command::new("/bin/sleep").arg("30"),
            std::time::Duration::from_millis(200),
        );
        let gave_up_after = started.elapsed();
        let limit = std::time::Duration::from_secs(5);

        assert_eq!(hung, CommandOutcome::TimedOut);
        assert!(gave_up_after < limit, "{gave_up_after:?}");
        assert_eq!(
            run_quiet_within(&mut Command::new("/usr/bin/true"), limit),
            CommandOutcome::Succeeded
        );
        assert_eq!(
            run_quiet_within(&mut Command::new("/usr/bin/false"), limit),
            CommandOutcome::Failed
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn should_trust_a_system_root_but_not_a_fresh_relay_ca_nobody_trusted() {
        let ca_dir = std::env::temp_dir().join(format!("relay-ca-trust-{}", uuid::Uuid::new_v4()));
        let ca = ensure_ca(&ca_dir).unwrap();
        let roots = Command::new("/usr/bin/security")
            .args([
                "find-certificate",
                "-a",
                "-p",
                "/System/Library/Keychains/SystemRootCertificates.keychain",
            ])
            .output()
            .unwrap()
            .stdout;

        let fresh_ca_trusted = UserCaTrust.trusts(&ca.cert_path);
        let some_root_trusted = decode_pem_blocks(&roots, "CERTIFICATE")
            .unwrap()
            .into_iter()
            .take(5)
            .enumerate()
            .any(|(index, der)| {
                let path = ca_dir.join(format!("root-{index}.der"));
                fs::write(&path, der).unwrap();
                UserCaTrust.trusts(&path)
            });
        fs::remove_dir_all(&ca_dir).unwrap();

        assert!(!fresh_ca_trusted);
        assert!(some_root_trusted);
    }
}
