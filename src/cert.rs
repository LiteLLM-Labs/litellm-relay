use std::{
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{anyhow, Result};
use rustls::{ClientConfig, RootCertStore, ServerConfig};

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

pub fn server_tls_config(host: &str, ca_dir: &Path) -> Result<ServerConfig> {
    let (cert_path, key_path) = ensure_leaf_cert(host, ca_dir)?;
    let cert_file = fs::read(&cert_path)?;
    let key_file = fs::read(&key_path)?;
    let certs = rustls_pemfile::certs(&mut Cursor::new(cert_file))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = rustls_pemfile::private_key(&mut Cursor::new(key_file))?
        .ok_or_else(|| anyhow!("leaf private key not found"))?;
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
}
