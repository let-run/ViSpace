//! No inventory or transfer credentials are sent until both peers prove an approved key.
use crate::{
    network_identity::{Membership, Proof},
    store::Store,
};
use anyhow::{Context, Result, bail, ensure};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, Weak};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Auth {
    AuthChallenge {
        version: u32,
        node_id: String,
        public_key: String,
        nonce: String,
    },
    AuthProof {
        proof: Proof,
    },
    AuthMembership {
        actions: Vec<Membership>,
    },
    AuthReady,
    AuthCodeReady,
    AuthCodeAccepted {
        server_key: Option<String>,
    },
}
async fn send<S: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    value: Auth,
) -> Result<()> {
    let text = serde_json::to_string(&value)?;
    ensure!(text.len() < 4 * 1024 * 1024, "Pairing message too large");
    tokio::time::timeout(
        std::time::Duration::from_secs(15),
        ws.send(Message::Text(text.into())),
    )
    .await??;
    Ok(())
}
async fn read<S: AsyncRead + AsyncWrite + Unpin>(ws: &mut WebSocketStream<S>) -> Result<Auth> {
    loop {
        match ws.next().await.context("Pairing disconnected")?? {
            Message::Text(t) => {
                ensure!(t.len() < 4 * 1024 * 1024, "Pairing message too large");
                return Ok(serde_json::from_str(&t)?);
            }
            Message::Ping(b) => ws.send(Message::Pong(b)).await?,
            Message::Pong(_) => {}
            _ => bail!("Expected authentication message"),
        }
    }
}
pub(crate) enum CodePairing {
    Host {
        binding: [u8; 32],
    },
    Client {
        binding: [u8; 32],
        server_pin: String,
        origin: String,
        status: std::sync::Arc<Mutex<crate::sync::ClientStatus>>,
        codes: tokio::sync::mpsc::Receiver<String>,
    },
}

impl CodePairing {
    fn binding(&self) -> &[u8; 32] {
        match self {
            Self::Host { binding } | Self::Client { binding, .. } => binding,
        }
    }

    fn code(&self) -> String {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(self.binding());
        format!(
            "{:04}",
            u32::from_be_bytes(digest[..4].try_into().unwrap()) % 10_000
        )
    }
}

pub(crate) async fn authenticate<S: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    store: Weak<Mutex<Store>>,
    expected: Option<&str>,
    approve_remote: bool,
    pairing: Option<CodePairing>,
) -> Result<String> {
    let shared = store.upgrade().context("Inventory closed")?;
    let identity = shared.lock().unwrap().network_identity();
    let local = identity.id()?;
    let nonce = format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    send(
        ws,
        Auth::AuthChallenge {
            version: 1,
            node_id: local.clone(),
            public_key: identity.public_key()?,
            nonce: nonce.clone(),
        },
    )
    .await?;
    let Auth::AuthChallenge {
        version,
        node_id: remote,
        public_key,
        nonce: remote_nonce,
    } = tokio::time::timeout(std::time::Duration::from_secs(15), read(ws)).await??
    else {
        bail!("Expected identity challenge")
    };
    ensure!(
        version == 1
            && remote != local
            && crate::network_identity::node_id(&public_key)? == remote
            && remote_nonce.len() >= 32
            && remote_nonce.len() <= 128,
        "Invalid node identity"
    );
    let mut payload = serde_json::json!({"version":1,"local":local,"remote":remote,"nonce":remote_nonce,"own_nonce":nonce});
    if let Some(pairing) = &pairing {
        payload["tls_binding"] = serde_json::json!(pairing.binding());
    }
    send(
        ws,
        Auth::AuthProof {
            proof: identity.sign("vinventory.hello.v1", &payload)?,
        },
    )
    .await?;
    let Auth::AuthProof { proof } =
        tokio::time::timeout(std::time::Duration::from_secs(15), read(ws)).await??
    else {
        bail!("Expected identity proof")
    };
    let mut payload = serde_json::json!({"version":1,"local":remote,"remote":local,"nonce":nonce,"own_nonce":remote_nonce});
    if let Some(pairing) = &pairing {
        payload["tls_binding"] = serde_json::json!(pairing.binding());
    }
    ensure!(
        crate::network_identity::verify("vinventory.hello.v1", &payload, &proof)? == remote
            && proof.public_key == public_key,
        "Invalid peer proof"
    );
    if let Some(expected) = expected {
        let matches = remote == expected
            || crate::discovery::hostname(&remote)
                .is_some_and(|host| host.eq_ignore_ascii_case(expected));
        ensure!(
            matches,
            "Discovered node identity does not match the authenticated key"
        );
    }
    let (_pairing, host_request) = {
        let mut store = shared.lock().unwrap();
        let host_request = pairing
            .as_ref()
            .filter(|pairing| matches!(pairing, CodePairing::Host { .. }))
            .map(|pairing| store.begin_pairing_code(&remote, pairing.code()))
            .transpose()?;
        store.network_seen(public_key.clone())?;
        if approve_remote && pairing.is_none() {
            store.network_authorize(public_key.clone(), false)?;
        }
        (store.begin_network_pairing(remote.clone()), host_request)
    };
    if let Some(pairing) = pairing {
        tokio::time::timeout(
            std::time::Duration::from_secs(120),
            verify_pairing_code(ws, &shared, &remote, &public_key, pairing, host_request),
        )
        .await
        .context("Pairing code expired")??;
    }
    let mut changes = shared.lock().unwrap().subscribe_changes();
    let mut peer_ready = false;
    let mut sent_ready = false;
    let mut sent_actions = String::new();
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(2));
    // A pending connection expires after ten minutes; there is no inventory access during pairing.
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(600));
    tokio::pin!(deadline);
    loop {
        let (actions, trusted) = {
            let s = shared.lock().unwrap();
            (s.network_actions(), s.network_trusted(&remote))
        };
        let serialized = serde_json::to_string(&actions)?;
        if !sent_ready && serialized != sent_actions {
            send(ws, Auth::AuthMembership { actions }).await?;
            sent_actions = serialized;
        }
        if trusted && !sent_ready {
            send(ws, Auth::AuthReady).await?;
            sent_ready = true;
        }
        if trusted && peer_ready {
            return Ok(remote);
        }
        tokio::select! {_= &mut deadline=>bail!("Pairing approval timed out"),_=heartbeat.tick()=>{},_=changes.changed()=>{},message=read(ws)=>match message?{
        Auth::AuthMembership{actions}=>{let shared=shared.clone();crate::async_io::blocking(move||shared.lock().unwrap().network_merge(actions)).await?;},
        Auth::AuthReady=>peer_ready=true,
        _=>bail!("Unexpected authentication message"),
        }}
    }
}

async fn verify_pairing_code<S: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    shared: &std::sync::Arc<Mutex<Store>>,
    remote: &str,
    public_key: &str,
    pairing: CodePairing,
    host_request: Option<(
        crate::network_security::CodeRequest,
        tokio::sync::watch::Receiver<bool>,
    )>,
) -> Result<()> {
    let expected = pairing.code();
    match pairing {
        CodePairing::Host { .. } => {
            let (_request, mut approval) = host_request.context("Missing host pairing request")?;
            if !*approval.borrow() {
                tokio::select! {
                    result = approval.wait_for(|approved| *approved) => { result?; }
                    _ = read(ws) => bail!("Peer disconnected before approval"),
                }
            }
            send(ws, Auth::AuthCodeReady).await?;
            let Auth::AuthCodeAccepted { server_key } = read(ws).await? else {
                bail!("Expected pairing confirmation");
            };
            {
                let mut store = shared.lock().unwrap();
                if let Some(pin) = &server_key {
                    ensure!(
                        pin.len() == 64 && pin.bytes().all(|b| b.is_ascii_hexdigit()),
                        "Invalid peer TLS key"
                    );
                }
                store.network_authorize(public_key.to_owned(), false)?;
                if let Some(pin) = server_key {
                    store.save_network_tls_pin(remote, None, &pin)?;
                }
            }
            send(ws, Auth::AuthCodeAccepted { server_key: None }).await?;
        }
        CodePairing::Client {
            server_pin,
            origin,
            status,
            mut codes,
            ..
        } => {
            *status.lock().unwrap() = crate::sync::ClientStatus::WaitingForApproval;
            ensure!(
                matches!(read(ws).await?, Auth::AuthCodeReady),
                "Expected host approval"
            );
            let mut verified = false;
            for remaining in (1..=3).rev() {
                *status.lock().unwrap() = crate::sync::ClientStatus::PairingCode { remaining };
                let entered = tokio::select! {
                    input = codes.recv() => input.context("Pairing cancelled")?,
                    _ = read(ws) => bail!("Pairing disconnected"),
                };
                if entered == expected {
                    verified = true;
                    break;
                }
            }
            ensure!(verified, "Incorrect pairing code. Reconnect to try again.");
            let server_key = crate::network_tls::local_server_pin(&shared.lock().unwrap()).ok();
            send(ws, Auth::AuthCodeAccepted { server_key }).await?;
            ensure!(
                matches!(read(ws).await?, Auth::AuthCodeAccepted { .. }),
                "Expected host confirmation"
            );
            let mut store = shared.lock().unwrap();
            store.network_authorize(public_key.to_owned(), false)?;
            store.save_network_tls_pin(remote, Some(&origin), &server_pin)?;
        }
    }
    Ok(())
}
