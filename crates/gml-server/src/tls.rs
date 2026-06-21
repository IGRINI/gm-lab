//! Self-signed TLS for LAN HTTPS — port of `tls_cert.py` to `rcgen`.
//!
//! Browsers expose the microphone only in a secure context (https / localhost),
//! so phone/tablet voice dictation over the LAN needs HTTPS. We generate a
//! long-lived self-signed cert (SANs = `localhost` + the machine's local IPv4
//! addresses) and cache it under a `.tls` dir, exactly like the Python helper.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair, SanType};

/// Errors raised while preparing the self-signed cert.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("tls io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("rcgen error: {0}")]
    Rcgen(#[from] rcgen::Error),
}

/// `local_ips()` — `127.0.0.1` plus every local address `local-ip-address`
/// reports. Sorted + deduped (like the Python `sorted(set(...))`).
pub fn local_ips() -> Vec<IpAddr> {
    let mut ips: std::collections::BTreeSet<IpAddr> = std::collections::BTreeSet::new();
    ips.insert("127.0.0.1".parse().unwrap());
    if let Ok(list) = local_ip_address::list_afinet_netifas() {
        for (_name, ip) in list {
            ips.insert(ip);
        }
    }
    if let Ok(ip) = local_ip_address::local_ip() {
        ips.insert(ip);
    }
    ips.into_iter().collect()
}

/// `lan_ipv4()` — local IPv4 addresses that are not loopback (for the printed
/// "open from your phone" URLs).
pub fn lan_ipv4() -> Vec<String> {
    local_ips()
        .into_iter()
        .filter_map(|ip| match ip {
            IpAddr::V4(v4) if !v4.is_loopback() => Some(v4.to_string()),
            _ => None,
        })
        .collect()
}

/// `ensure_self_signed(cert_dir)` -> (cert_path, key_path).
///
/// Returns the cached PEM pair if it already exists; otherwise generates a
/// self-signed cert (CN `gm-lab.local`, SAN `localhost` + local IPs) valid for
/// ~10 years and writes both PEMs.
pub fn ensure_self_signed(cert_dir: &Path) -> Result<(PathBuf, PathBuf), TlsError> {
    std::fs::create_dir_all(cert_dir)?;
    let cert_path = cert_dir.join("gmlab-cert.pem");
    let key_path = cert_dir.join("gmlab-key.pem");
    if cert_path.exists() && key_path.exists() {
        return Ok((cert_path, key_path));
    }

    let mut params = CertificateParams::default();
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "gm-lab.local");
    params.distinguished_name = dn;

    let mut sans = vec![SanType::DnsName("localhost".try_into()?)];
    for ip in local_ips() {
        sans.push(SanType::IpAddress(ip));
    }
    params.subject_alt_names = sans;

    let key_pair = KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;

    std::fs::write(&cert_path, cert.pem())?;
    std::fs::write(&key_path, key_pair.serialize_pem())?;
    Ok((cert_path, key_path))
}
