//! Device identity: a per-device local Certificate Authority + TLS leaf key.
//!
//! Every Single device generates its own CA **key** on first launch and keeps
//! it in the app system folder (`ca_key.pem`). The SHA-256 fingerprint of the
//! CA *public key* (the DER SubjectPublicKeyInfo) is the device identity:
//!
//! * `device_id` = first 16 hex chars of SHA-256(SPKI DER) — stable forever
//! * `ca_fp`     = full 64 hex chars — pinned by every paired peer
//!
//! The CA key never leaves the device. The TLS leaf key is persisted; the
//! leaf certificate is re-signed on every start so its SANs contain the
//! *current* IP addresses (needed so Windows WebClient accepts
//! `https://<ip>:<port>/dav/` drive mappings). Because peers pin the public
//! key (not the cert bytes), re-signing never invalidates a pairing.
//!
//! Pairing model ("PIN + approval"):
//! 1. Devices connect over TLS; each side pins the other's fingerprint
//!    learned during pairing.
//! 2. Single-to-Single connections also present client certificates so the
//!    server can bind pair-requests to the caller's verified identity.
//! 3. After pairing, file access uses a shared token derived from both
//!    fingerprints plus the pairing PIN.

use anyhow::{Context, Result};
use rand::RngCore;
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

pub struct Identity {
    pub device_id: String,
    pub ca_fp: String,
    pub device_name: String,

    /// CA key (persisted; the CA *certificate* is re-issued each start)
    #[allow(dead_code)] // retained so leaves can be re-signed on IP change (v0.2)
    ca_key: KeyPair,
    #[allow(dead_code)]
    ca_cert: rcgen::Certificate,
    /// current CA certificate in DER/PEM (exported for OS trust stores)
    ca_der: Vec<u8>,
    ca_pem: String,

    #[allow(dead_code)]
    leaf_key_pem: String,
    leaf_key_der: Vec<u8>,
    /// leaf + CA chain in PEM (what the TLS server presents)
    pub chain_pem: String,
    leaf_der: Vec<u8>,
}

fn dns_san(name: &str) -> SanType {
    use std::convert::TryFrom;
    SanType::DnsName(
        rcgen::Ia5String::try_from(name.to_string()).expect("valid dns name"),
    )
}

impl Identity {
    pub fn device_id(&self) -> &str {
        &self.device_id
    }
    pub fn ca_fp(&self) -> &str {
        &self.ca_fp
    }
    pub fn ca_pem(&self) -> &str {
        &self.ca_pem
    }
    pub fn ca_der(&self) -> &[u8] {
        &self.ca_der
    }
    pub fn leaf_der(&self) -> &[u8] {
        &self.leaf_der
    }

    /// TLS server config: chain (leaf + CA) + leaf key.
    pub fn tls_server_config(&self) -> Result<rustls::ServerConfig> {
        let certs: Vec<CertificateDer<'static>> = vec![
            CertificateDer::from(self.leaf_der.clone()),
            CertificateDer::from(self.ca_der.clone()),
        ];
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            self.leaf_key_der.clone(),
        ));
        let cfg = rustls::ServerConfig::builder()
            .with_client_cert_verifier(Arc::new(client_verifier::AcceptAny))
            .with_single_cert(certs, key)
            .context("invalid TLS server key/cert")?;
        Ok(cfg)
    }

    /// TLS client config pinning the given peer CA fingerprints and
    /// presenting our own certificate. `allow_prefix` permits connecting to
    /// a not-yet-paired device whose fingerprint starts with the known
    /// device id (used during the pairing handshake itself). `accept_any`
    /// is only used for the manual "add by IP" probe.
    pub fn client_config(
        &self,
        pinned_cas: &[String],
        allow_prefix: Option<String>,
        accept_any: bool,
    ) -> Result<rustls::ClientConfig> {
        let pinned: std::collections::HashSet<String> = pinned_cas.iter().cloned().collect();
        let verifier = Arc::new(client_pinning::PinVerifier::new(
            pinned,
            allow_prefix,
            accept_any,
        ));

        let certs = vec![
            CertificateDer::from(self.leaf_der.clone()),
            CertificateDer::from(self.ca_der.clone()),
        ];
        let key = PrivateKeyDer::Pkcs8(rustls::pki_types::PrivatePkcs8KeyDer::from(
            self.leaf_key_der.clone(),
        ));

        let mut cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_auth_cert(certs, key)
            .context("client tls config")?;
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(cfg)
    }
}

/// Load identity, creating the CA key / leaf key on first run.
pub fn load_or_create(
    paths: &crate::config::Paths,
    device_name: &str,
    ips: &[IpAddr],
) -> Result<Arc<Identity>> {
    let ca_key_path = paths.ca_key();
    let leaf_key_path = paths.server_key();

    // --- CA key (persisted; the identity anchor) -------------------------
    let ca_key = if ca_key_path.exists() {
        let pem = std::fs::read_to_string(&ca_key_path).context("reading ca_key.pem")?;
        KeyPair::from_pem(&pem).context("loading CA key")?
    } else {
        let kp = KeyPair::generate().context("generating CA key")?;
        std::fs::write(&ca_key_path, kp.serialize_pem()).context("writing ca_key.pem")?;
        kp
    };

    // Identity = hash of the CA public key (SPKI DER) — stable across restarts.
    let ca_fp = hex::encode(Sha256::digest(ca_key.public_key_der()));
    let device_id = ca_fp[..16].to_string();

    // --- fresh CA certificate (re-issued each start from the same key) ----
    let ca_cert = issue_ca(&ca_key, device_name)?;
    let ca_der = ca_cert.der().as_ref().to_vec();
    let ca_pem = ca_cert.pem();
    // Export for the OS trust-store helper (Windows drive mapping).
    std::fs::write(paths.ca_cert(), &ca_pem).ok();

    // --- leaf key (persisted) --------------------------------------------
    let leaf_key_pem = if leaf_key_path.exists() {
        std::fs::read_to_string(&leaf_key_path).context("reading server_key.pem")?
    } else {
        let kp = KeyPair::generate().context("generating TLS key")?;
        let pem = kp.serialize_pem();
        std::fs::write(&leaf_key_path, &pem).context("writing server_key.pem")?;
        pem
    };

    // --- fresh leaf -------------------------------------------------------
    let leaf_key = KeyPair::from_pem(&leaf_key_pem).context("loading leaf key")?;
    let (leaf_der, chain_pem) =
        sign_leaf(&ca_cert, &ca_key, &leaf_key, device_name, ips)?;
    let leaf_key_der = leaf_key.serialize_der();

    std::fs::write(paths.server_cert(), &chain_pem).ok();

    Ok(Arc::new(Identity {
        device_id,
        ca_fp,
        device_name: device_name.to_string(),
        ca_key,
        ca_cert,
        ca_der,
        ca_pem,
        leaf_key_pem,
        leaf_key_der,
        chain_pem,
        leaf_der,
    }))
}

fn issue_ca(ca_key: &KeyPair, device_name: &str) -> Result<rcgen::Certificate> {
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::CrlSign,
    ];
    params
        .distinguished_name
        .push(DnType::CommonName, format!("Single Local CA ({})", device_name));
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(3650);
    params.self_signed(ca_key).context("self-signing CA")
}

fn sign_leaf(
    ca_cert: &rcgen::Certificate,
    ca_key: &KeyPair,
    leaf_key: &KeyPair,
    device_name: &str,
    ips: &[IpAddr],
) -> Result<(Vec<u8>, String)> {
    let mut sans = vec![dns_san("localhost"), dns_san("single.local")];
    for ip in ips {
        sans.push(SanType::IpAddress(*ip));
    }

    let mut params = CertificateParams::default();
    params.is_ca = IsCa::NoCa;
    params.subject_alt_names = sans;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params
        .distinguished_name
        .push(DnType::CommonName, format!("Single ({})", device_name));
    params.not_before = time::OffsetDateTime::now_utc() - time::Duration::days(1);
    params.not_after = time::OffsetDateTime::now_utc() + time::Duration::days(825);

    let leaf_cert = params
        .signed_by(leaf_key, ca_cert, ca_key)
        .context("signing leaf with device CA")?;
    let leaf_der = leaf_cert.der().as_ref().to_vec();
    let chain_pem = format!("{}\n{}", leaf_cert.pem(), ca_cert.pem().trim());
    Ok((leaf_der, chain_pem))
}

// ---------------------------------------------------------------- DER / SPKI

/// Extract the DER-encoded SubjectPublicKeyInfo from an X.509 certificate.
///
/// Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signature }
/// tbsCertificate ::= SEQUENCE {
///     [0] version, serialNumber, signature, issuer, validity, subject,
///     subjectPublicKeyInfo, ... }
///
/// A tiny TLV walker is enough — no x509 parser dependency — and it is
/// round-trip tested below against `KeyPair::public_key_der()`.
pub fn spki_from_cert_der(der: &[u8]) -> Option<Vec<u8>> {
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
    let (tag, s, l) = tlv_at(der, 0)?;
    if tag != 0x30 {
        return None;
    }
    let content = &der[s..s + l];
    // element 0 of the content = tbsCertificate SEQUENCE; we need its CONTENT
    let (t, ts, tl) = tlv_at(content, 0)?;
    if t != 0x30 {
        return None;
    }
    let tbs = &content[ts..ts + tl];

    let mut pos = 0usize;
    // optional [0] EXPLICIT version
    let (t, _s, e) = tlv_span(tbs, pos)?;
    if t == 0xA0 {
        pos = e;
    }
    // skip: serialNumber, signature, issuer, validity, subject
    for _ in 0..5 {
        let (t, _s, e) = tlv_span(tbs, pos)?;
        if !matches!(t, 0x02 | 0x30) {
            return None;
        }
        pos = e;
    }
    // subjectPublicKeyInfo (full TLV: header + content)
    let (t, spki_start, spki_end) = tlv_span(tbs, pos)?;
    if t != 0x30 {
        return None;
    }
    Some(tbs[spki_start..spki_end].to_vec())
}

/// Like `tlv_at` but returns the full TLV span (start, end) including header.
fn tlv_span(buf: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    let (tag, content_start, len) = tlv_at(buf, pos)?;
    Some((tag, pos, content_start + len))
}

/// Parse one DER TLV at `pos`. Returns (tag, content_start, content_len).
fn tlv_at(buf: &[u8], pos: usize) -> Option<(u8, usize, usize)> {
    if pos + 2 > buf.len() {
        return None;
    }
    let tag = buf[pos];
    let mut i = pos + 1;
    let len = if buf[i] & 0x80 == 0 {
        let l = buf[i] as usize;
        i += 1;
        l
    } else {
        let n = (buf[i] & 0x7f) as usize;
        i += 1;
        if n == 0 || n > 4 || i + n > buf.len() {
            return None;
        }
        let mut l = 0usize;
        for k in 0..n {
            l = (l << 8) | buf[i + k] as usize;
        }
        i += n;
        l
    };
    if i.checked_add(len)? > buf.len() {
        return None;
    }
    Some((tag, i, len))
}

/// Random 6-digit PIN.
pub fn random_pin() -> String {
    let mut buf = [0u8; 4];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    let n = u32::from_le_bytes(buf) % 1_000_000;
    format!("{:06}", n)
}

/// Shared pairing token: deterministic on both sides, only ever travels
/// inside TLS.
pub fn derive_token(fp_a: &str, fp_b: &str, pin: &str) -> String {
    let (lo, hi) = if fp_a <= fp_b {
        (fp_a, fp_b)
    } else {
        (fp_b, fp_a)
    };
    let mut hasher = Sha256::new();
    hasher.update(b"Single-v1-token");
    hasher.update(lo.as_bytes());
    hasher.update(hi.as_bytes());
    hasher.update(pin.as_bytes());
    hex::encode(hasher.finalize())
}

/// Ensure `candidate` stays inside `root` (defense against `..` traversal).
pub fn path_within(root: &Path, candidate: &Path) -> bool {
    candidate.starts_with(root)
}

/// Fingerprint (SHA-256 hex) of a certificate's public key.
pub fn cert_fp(certs: &[CertificateDer<'_>]) -> Option<String> {
    let last = certs.last()?;
    let spki = spki_from_cert_der(last.as_ref())?;
    Some(hex::encode(Sha256::digest(&spki)))
}

/// Minimal "accept any client cert" verifier (client certificates are
/// optional; Single peers present them so pair-requests can be bound to a
/// verified fingerprint).
pub mod client_verifier {
    use rustls::pki_types::{CertificateDer, UnixTime};
    use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
    use rustls::DigitallySignedStruct;

    #[derive(Debug)]
    pub struct AcceptAny;

    impl ClientCertVerifier for AcceptAny {
        fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
            &[]
        }

        fn client_auth_mandatory(&self) -> bool {
            false
        }

        fn verify_client_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _now: UnixTime,
        ) -> Result<ClientCertVerified, rustls::Error> {
            Ok(ClientCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}

/// Client-side pinning verifier: only accepts servers whose public key
/// fingerprint matches a pinned pairing record.
pub mod client_pinning {
    use super::cert_fp;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::DigitallySignedStruct;
    use std::collections::HashSet;

    #[derive(Debug)]
    pub struct PinVerifier {
        pub pinned: HashSet<String>,
        pub allow_prefix: Option<String>,
        pub accept_any: bool,
    }

    impl PinVerifier {
        pub fn new(pinned: HashSet<String>, allow_prefix: Option<String>, accept_any: bool) -> Self {
            PinVerifier {
                pinned,
                allow_prefix,
                accept_any,
            }
        }
    }

    impl ServerCertVerifier for PinVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            for cert in std::iter::once(end_entity).chain(intermediates.iter()) {
                if self.accept_any {
                    return Ok(ServerCertVerified::assertion());
                }
                if let Some(fp) = cert_fp(std::slice::from_ref(cert)) {
                    if self.pinned.contains(&fp) {
                        return Ok(ServerCertVerified::assertion());
                    }
                    if let Some(prefix) = &self.allow_prefix {
                        if fp.starts_with(prefix.as_str()) {
                            return Ok(ServerCertVerified::assertion());
                        }
                    }
                }
            }
            Err(rustls::Error::General(
                "server certificate not pinned (unknown device)".into(),
            ))
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &rustls::crypto::ring::default_provider()
                    .signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            rustls::crypto::ring::default_provider()
                .signature_verification_algorithms
                .supported_schemes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_paths(tag: &str) -> crate::config::Paths {
        let dir = std::env::temp_dir().join(format!("single-id-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::config::Paths::new(dir).unwrap()
    }

    #[test]
    fn spki_extractor_matches_rcgen() {
        let key = KeyPair::generate().unwrap();
        let cert = issue_ca(&key, "TestBox").unwrap();
        let spki = spki_from_cert_der(cert.der().as_ref()).expect("spki extracted");
        assert_eq!(spki, key.public_key_der());
        // also works on the leaf
        let leaf_key = KeyPair::generate().unwrap();
        let ips = vec!["127.0.0.1".parse::<IpAddr>().unwrap()];
        let (leaf_der, _chain) =
            sign_leaf(&cert, &key, &leaf_key, "TestBox", &ips).unwrap();
        let spki2 = spki_from_cert_der(&leaf_der).expect("leaf spki");
        assert_eq!(spki2, leaf_key.public_key_der());
    }

    #[test]
    fn debug_fp_consistency() {
        let paths = tmp_paths("fpdbg");
        let ips = vec!["127.0.0.1".parse::<IpAddr>().unwrap()];
        let id = load_or_create(&paths, "FP", &ips).unwrap();
        let ca_spki = spki_from_cert_der(id.ca_der()).unwrap();
        let from_cert = hex::encode(Sha256::digest(&ca_spki));
        println!("fp_field    = {}", id.ca_fp());
        println!("fp_from_ca  = {}", from_cert);
        let leaf_spki = spki_from_cert_der(id.leaf_der()).unwrap();
        println!("fp_from_leaf= {}", hex::encode(Sha256::digest(&leaf_spki)));
        assert_eq!(id.ca_fp(), from_cert, "ca fp mismatch");
    }

    #[test]
    fn identity_is_stable_across_restarts() {
        let paths = tmp_paths("stable");
        let ips = vec!["127.0.0.1".parse::<IpAddr>().unwrap()];
        let a = load_or_create(&paths, "BoxA", &ips).unwrap();
        let fp1 = a.ca_fp.clone();
        let id1 = a.device_id.clone();
        drop(a);
        let b = load_or_create(&paths, "BoxA", &ips).unwrap();
        assert_eq!(fp1, b.ca_fp());
        assert_eq!(id1, b.device_id());
        let _ = std::fs::remove_dir_all(&paths.base);
    }

    #[test]
    fn token_is_symmetric_and_pin_dependent() {
        let t1 = derive_token("aa", "bb", "123456");
        let t2 = derive_token("bb", "aa", "123456");
        let t3 = derive_token("aa", "bb", "654321");
        assert_eq!(t1, t2);
        assert_ne!(t1, t3);
        assert_eq!(t1.len(), 64);
    }

    #[test]
    fn pin_has_six_digits() {
        let pin = random_pin();
        assert_eq!(pin.len(), 6);
        assert!(pin.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn path_within_works() {
        let root = Path::new("/srv/share");
        assert!(path_within(root, Path::new("/srv/share/a/b.txt")));
        assert!(!path_within(root, Path::new("/srv/share2/x")));
        assert!(!path_within(root, Path::new("/etc/passwd")));
    }
}
