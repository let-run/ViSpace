//! TLS uses the OS trust store on clients and a local CA for automatic listeners.
use anyhow::{Context, Result};
use std::{io::Write, path::PathBuf, sync::Arc};
use tokio_rustls::rustls::{
    self,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    pub certificate: PathBuf,
    pub private_key: PathBuf,
}
pub fn path(store: &crate::store::Store) -> PathBuf {
    store.path.with_extension("tls.json")
}
pub fn load(store: &crate::store::Store) -> Result<Settings> {
    match std::fs::read(path(store)) {
        Ok(bytes) => {
            let mut settings: Settings = serde_json::from_slice(&bytes)?;
            settings.certificate = relocated_local_tls_path(settings.certificate);
            settings.private_key = relocated_local_tls_path(settings.private_key);
            Ok(settings)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(test)]
            {
                return Ok(test_settings());
            }
            #[cfg(not(test))]
            Ok(Settings {
                certificate: std::env::var_os("VINVENTORY_TLS_CERT")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
                private_key: std::env::var_os("VINVENTORY_TLS_KEY")
                    .map(PathBuf::from)
                    .unwrap_or_default(),
            })
        }
        Err(e) => Err(e.into()),
    }
}
// Existing listeners may have saved absolute paths before the workspace moved.
fn relocated_local_tls_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        if let Ok(relative) = path.strip_prefix(root.join("rust/.local-tls")) {
            let relocated = root.join(".local-tls").join(relative);
            if relocated.is_file() {
                return relocated;
            }
        }
    }
    path
}

pub fn save(store: &crate::store::Store, settings: &Settings) -> Result<()> {
    if !settings.is_automatic() {
        let _ = acceptor(settings)?;
    }
    let destination = path(store);
    let mut file = tempfile::NamedTempFile::new_in(
        destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(std::path::Path::new(".")),
    )?;
    serde_json::to_writer(&mut file, settings)?;
    file.as_file().sync_all()?;
    file.persist(destination)?;
    Ok(())
}

impl Settings {
    pub fn is_automatic(&self) -> bool {
        self.certificate.as_os_str().is_empty() && self.private_key.as_os_str().is_empty()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LocalCa {
    certificate: String,
    private_key: String,
}

fn ca_path(store: &crate::store::Store) -> PathBuf {
    store.path.with_extension("tls-ca.json")
}

pub fn public_ca(store: &crate::store::Store) -> Result<String> {
    let ca: LocalCa = serde_json::from_slice(
        &std::fs::read(ca_path(store))
            .context("Start the automatic listener to generate its CA certificate")?,
    )?;
    Ok(ca.certificate)
}

fn local_ca(path: &std::path::Path) -> Result<LocalCa> {
    match std::fs::read(path) {
        Ok(bytes) => return Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut params = rcgen::CertificateParams::default();
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3650);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "ViSpace Local Sync CA");
    params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Constrained(0));
    params.key_usages = vec![
        rcgen::KeyUsagePurpose::KeyCertSign,
        rcgen::KeyUsagePurpose::CrlSign,
    ];
    let key = rcgen::KeyPair::generate()?;
    let ca = LocalCa {
        certificate: params.self_signed(&key)?.pem(),
        private_key: key.serialize_pem(),
    };
    // NamedTempFile creates a private file on Unix. Keep the CA and its key together
    // so an interrupted write cannot replace one without the other.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, &ca)?;
    file.flush()?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_) => Ok(ca),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(serde_json::from_slice(&std::fs::read(path)?)?)
        }
        Err(error) => Err(error.into()),
    }
}

pub fn listener(
    store: &crate::store::Store,
    hostname: Option<&str>,
    ip: std::net::IpAddr,
) -> Result<(tokio_rustls::TlsAcceptor, bool)> {
    let settings = load(store)?;
    if !settings.is_automatic() {
        let cert = CertificateDer::from_pem_file(&settings.certificate)?;
        return Ok((
            acceptor(&settings)?,
            hostname.is_some_and(|host| covers_hostname(&cert, host)),
        ));
    }
    let ca = local_ca(&ca_path(store))?;
    let issuer = rcgen::Issuer::from_ca_cert_pem(
        &ca.certificate,
        rcgen::KeyPair::from_pem(&ca.private_key)?,
    )?;
    let mut names = vec!["localhost".into(), "127.0.0.1".into(), "::1".into()];
    if let Some(host) = hostname {
        names.push(host.into());
    }
    if !ip.is_unspecified() && !ip.is_loopback() {
        names.push(ip.to_string());
    }
    let mut params = rcgen::CertificateParams::new(names)?;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(365);
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, hostname.unwrap_or("localhost"));
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = server_key(store)?;
    let cert = params.signed_by(&key, &issuer)?;
    let covers = hostname.is_some_and(|host| covers_hostname(cert.der(), host));
    let key = PrivateKeyDer::Pkcs8(key.serialize_der().into());
    let certs = vec![
        cert.der().clone(),
        CertificateDer::from_pem_slice(ca.certificate.as_bytes())?,
    ];
    Ok((acceptor_with_chain(certs, key)?, covers))
}

fn server_key(store: &crate::store::Store) -> Result<rcgen::KeyPair> {
    let path = store.path.with_extension("tls-server-key.pem");
    match std::fs::read_to_string(&path) {
        Ok(pem) => return Ok(rcgen::KeyPair::from_pem(&pem)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let key = rcgen::KeyPair::generate()?;
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(key.serialize_pem().as_bytes())?;
    file.as_file().sync_all()?;
    match file.persist_noclobber(&path) {
        Ok(_) => Ok(key),
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            Ok(rcgen::KeyPair::from_pem(&std::fs::read_to_string(path)?)?)
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn fingerprint(cert: &CertificateDer<'_>) -> Result<String, rustls::Error> {
    let parsed = rustls::server::ParsedCertificate::try_from(cert)?;
    Ok(key_digest(parsed.subject_public_key_info().as_ref()))
}

pub(crate) fn local_server_pin(store: &crate::store::Store) -> Result<String> {
    use rcgen::PublicKeyData;
    let settings = load(store)?;
    if settings.is_automatic() {
        let key = server_key(store)?;
        Ok(key_digest(&key.subject_public_key_info()))
    } else {
        Ok(fingerprint(&CertificateDer::from_pem_file(
            settings.certificate,
        )?)?)
    }
}

fn key_digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(crate) const PAIRING_EXPORTER: &[u8] = b"EXPORTER-ViSpace-Pairing-v1";

#[derive(Debug)]
pub(crate) enum PeerTls {
    System,
    Pairing,
    Pinned(String),
}

#[derive(Debug)]
struct PairingVerifier {
    pin: Option<String>,
    algorithms: rustls::crypto::WebPkiSupportedAlgorithms,
}

impl rustls::client::danger::ServerCertVerifier for PairingVerifier {
    fn verify_server_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let key = fingerprint(cert)?;
        if self.pin.as_ref().is_some_and(|pin| *pin != key) {
            return Err(rustls::Error::General(
                "Paired server TLS key changed".into(),
            ));
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, signature, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, signature, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

fn covers_hostname(cert: &CertificateDer<'_>, hostname: &str) -> bool {
    (|| -> Result<()> {
        let parsed = rustls::server::ParsedCertificate::try_from(cert)?;
        rustls::client::verify_server_name(
            &parsed,
            &rustls::pki_types::ServerName::try_from(hostname)?,
        )?;
        Ok(())
    })()
    .is_ok()
}

pub fn acceptor(settings: &Settings) -> Result<tokio_rustls::TlsAcceptor> {
    let cert = CertificateDer::pem_file_iter(&settings.certificate)
        .context("Choose the WSS certificate PEM file in Preferences → Sync")?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let key = PrivateKeyDer::from_pem_file(&settings.private_key)
        .context("Could not read WSS private key")?;
    acceptor_with_chain(cert, key)
}

fn acceptor_with_chain(
    cert: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<tokio_rustls::TlsAcceptor> {
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(cert, key)?;
    Ok(tokio_rustls::TlsAcceptor::from(Arc::new(config)))
}
type ClientSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type ClientResponse = tokio_tungstenite::tungstenite::handshake::client::Response;

pub(crate) async fn connect_peer(
    url: &str,
    config: Option<tokio_tungstenite::tungstenite::protocol::WebSocketConfig>,
    disable_nagle: bool,
    trust: PeerTls,
) -> Result<(ClientSocket, ClientResponse)> {
    use rustls_platform_verifier::BuilderVerifierExt;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()?;
    #[allow(unused_mut)]
    let mut config_tls = match &trust {
        PeerTls::System => builder.with_platform_verifier()?.with_no_client_auth(),
        PeerTls::Pairing | PeerTls::Pinned(_) => {
            let pin = match &trust {
                PeerTls::Pinned(key) => Some(key.clone()),
                _ => None,
            };
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(PairingVerifier {
                    pin,
                    algorithms: provider.signature_verification_algorithms,
                }))
                .with_no_client_auth()
        }
    };
    #[cfg(test)]
    if let (PeerTls::System, Some(ca)) = (&trust, TEST_CA.get()) {
        let mut roots = rustls::RootCertStore::empty();
        for certificate in
            CertificateDer::pem_file_iter(ca)?.collect::<std::result::Result<Vec<_>, _>>()?
        {
            roots.add(certificate)?;
        }
        config_tls = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    }
    let parsed = url::Url::parse(url)?;
    if matches!(trust, PeerTls::Pairing) {
        anyhow::ensure!(
            parsed.path() == "/pair",
            "Unverified TLS is restricted to pairing"
        );
    }
    let connector = Some(tokio_tungstenite::Connector::Rustls(Arc::new(config_tls)));
    if let Some(host) = parsed
        .host_str()
        .filter(|host| host.ends_with(".vispace.local"))
    {
        let addresses = crate::discovery::resolve(
            host,
            parsed.port_or_known_default().context("Missing WSS port")?,
        )
        .await?;
        for address in addresses {
            if let Ok(Ok(stream)) = tokio::time::timeout(
                std::time::Duration::from_secs(2),
                tokio::net::TcpStream::connect(address),
            )
            .await
            {
                if disable_nagle {
                    stream.set_nodelay(true)?;
                }
                return Ok(tokio_tungstenite::client_async_tls_with_config(
                    url, stream, config, connector,
                )
                .await?);
            }
        }
        anyhow::bail!("Could not reach the discovered host {host}");
    }
    Ok(
        tokio_tungstenite::connect_async_tls_with_config(url, config, disable_nagle, connector)
            .await?,
    )
}

pub(crate) fn client_pairing_binding(socket: &ClientSocket) -> Result<([u8; 32], String)> {
    let tokio_tungstenite::MaybeTlsStream::Rustls(stream) = socket.get_ref() else {
        anyhow::bail!("Pairing requires TLS");
    };
    let connection = stream.get_ref().1;
    let binding = connection.export_keying_material([0; 32], PAIRING_EXPORTER, None)?;
    let certificate = connection
        .peer_certificates()
        .and_then(|certs| certs.first())
        .context("Missing server certificate")?;
    Ok((binding, fingerprint(certificate)?))
}

pub(crate) fn peer_trust(
    store: &std::sync::Weak<std::sync::Mutex<crate::store::Store>>,
    url: &url::Url,
    peer: Option<&str>,
) -> Result<PeerTls> {
    let peer = peer.or_else(|| url.host_str()?.strip_suffix(".vispace.local"));
    let shared = store.upgrade().context("Inventory closed")?;
    let store = shared.lock().unwrap();
    Ok(
        match store.network_tls_pin(&url.origin().ascii_serialization(), peer)? {
            Some((_, pin)) => PeerTls::Pinned(pin),
            None => PeerTls::System,
        },
    )
}

#[cfg(test)]
pub(crate) static TEST_CA: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) fn test_settings() -> Settings {
    static FIXTURE: std::sync::OnceLock<(tempfile::TempDir, Settings)> = std::sync::OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let temp = tempfile::tempdir().unwrap();
            let dir = temp.path().join("tls");
            let status = std::process::Command::new("python3")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/script/generate_test_certificates.py"
                ))
                .arg("--directory")
                .arg(&dir)
                .status()
                .unwrap();
            assert!(status.success());
            TEST_CA.set(dir.join("ca.pem")).unwrap();
            let settings = Settings {
                certificate: dir.join("server.pem"),
                private_key: dir.join("server-key.pem"),
            };
            (temp, settings)
        })
        .1
        .clone()
}

#[cfg(test)]
mod automatic_tests {
    use super::*;

    #[tokio::test]
    async fn automatic_certificate_is_trusted_after_listener_restart() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let store = crate::store::Store::open(directory.path().join("inventory.sqlite"))?;
        save(&store, &Settings::default())?;
        let hostname = crate::discovery::hostname(store.application_id()).unwrap();
        let ip = std::net::Ipv4Addr::LOCALHOST.into();
        let (_, covers) = listener(&store, Some(&hostname), ip)?;
        assert!(covers);
        let ca = public_ca(&store)?;

        let mut roots = rustls::RootCertStore::empty();
        roots.add(CertificateDer::from_pem_slice(ca.as_bytes())?)?;
        let client = tokio_rustls::TlsConnector::from(Arc::new(
            rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth(),
        ));

        let (server, _) = listener(&store, Some(&hostname), ip)?;
        assert_eq!(ca, public_ca(&store)?);
        for name in [hostname, "localhost".into(), "127.0.0.1".into()] {
            let (client_io, server_io) = tokio::io::duplex(8192);
            let (client_result, server_result) = tokio::join!(
                client.connect(rustls::pki_types::ServerName::try_from(name)?, client_io),
                server.accept(server_io)
            );
            client_result?;
            server_result?;
        }
        Ok(())
    }
}
