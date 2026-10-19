//! The control channel inside a management tunnel (design D4, D25).
//!
//! The Core speaks to an access point at the access point's tunnel address.
//! The access point trusts a datagram by its source address alone: inside
//! the tunnel, WireGuard's cryptokey routing binds that address to the
//! Core's key, and the access point's firewall admits the port from the
//! tunnel zone only.

use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use uciedit::{dump_all, parse_all, Arena};

use crate::prelude::*;
use crate::satellite::{self, RouterRole};

pub const PORT: u16 = 8720;
const REPLY_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DATAGRAM: usize = 1024;

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Message {
    /// The Core has forgotten this access point.
    Release,
    Released,
}

static RUNNING: Mutex<bool> = Mutex::new(false);

/// Listens on an adopted access point. Idempotent across re-adoptions.
pub fn start() {
    let role = satellite::load_role();
    if role.role != RouterRole::Satellite || role.core_key.is_none() {
        return;
    }
    {
        let mut running = RUNNING.lock().unwrap();
        if *running {
            return;
        }
        *running = true;
    }
    // Release rewrites UCI and holds a uciedit `Arena` across awaits (!Send).
    if let Err(e) = std::thread::Builder::new()
        .name("control".into())
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("control runtime")
                .block_on(async {
                    if let Err(e) = run_ap().await {
                        tracing::error!("control: listener failed: {e}");
                    }
                    *RUNNING.lock().unwrap() = false;
                });
        })
    {
        tracing::error!("control: failed to start listener thread: {e}");
        *RUNNING.lock().unwrap() = false;
    }
}

fn core_address() -> Option<IpAddr> {
    satellite::load_role()
        .core_address
        .and_then(|a| a.parse().ok())
}

async fn run_ap() -> Result<(), Error> {
    let socket = UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], PORT)))
        .await
        .with_kind(ErrorKind::Network)?;
    tracing::info!("control: listening for the Core on port {PORT}");
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let Ok((n, src)) = socket.recv_from(&mut buf).await else {
            continue;
        };
        if Some(src.ip()) != core_address() {
            tracing::debug!("control: ignored a datagram from {}", src.ip());
            continue;
        }
        let Ok(msg) = serde_json::from_slice::<Message>(&buf[..n]) else {
            continue;
        };
        match msg {
            Message::Release => {
                let _ = socket
                    .send_to(&serde_json::to_vec(&Message::Released).unwrap(), src)
                    .await;
                match release_self().await {
                    Ok(()) => {
                        tracing::info!("control: released by the Core; waiting to be adopted")
                    }
                    Err(e) => tracing::error!("control: release failed: {e}"),
                }
            }
            Message::Released => {}
        }
    }
}

/// Back to a candidate: the tunnel goes, the base layout stays, and the
/// candidate listener answers the next Core on the backhaul.
async fn release_self() -> Result<(), Error> {
    {
        let arena = Arena::new();
        let mut cfgs = parse_all("/etc/config", &arena, &["network", "firewall", "system"]).await?;
        crate::mgmt_tunnel::remove_ap_end(&mut cfgs)?;
        dump_all("/etc/config", cfgs).await?;
    }
    let mut role = satellite::load_role();
    role.core_key = None;
    role.core_endpoint = None;
    role.core_address = None;
    role.generation = 0;
    satellite::persist_role(&role).await?;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/network").arg("reload"))
            .await;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/firewall").arg("reload"))
            .await;
    let _ =
        crate::run_quiet_async(tokio::process::Command::new("/etc/init.d/sysntpd").arg("restart"))
            .await;
    crate::activity::log(
        "satellite",
        "released",
        true,
        "Released by the Core; waiting to be adopted again",
        None,
    );
    crate::adoption::start();
    Ok(())
}

/// Tells the access point at `addr` that the Core has forgotten it. `Ok(false)`
/// when it did not answer in time: a dark access point learns the same from
/// its tunnel never coming back.
pub async fn release(addr: IpAddr) -> Result<bool, Error> {
    let bind: SocketAddr = match addr {
        IpAddr::V4(_) => ([0, 0, 0, 0], 0).into(),
        IpAddr::V6(_) => ([0u16; 8], 0).into(),
    };
    let socket = UdpSocket::bind(bind).await.with_kind(ErrorKind::Network)?;
    socket
        .send_to(
            &serde_json::to_vec(&Message::Release).unwrap(),
            SocketAddr::new(addr, PORT),
        )
        .await
        .with_kind(ErrorKind::Network)?;
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let reply = tokio::time::timeout(REPLY_TIMEOUT, socket.recv_from(&mut buf)).await;
    Ok(match reply {
        Ok(Ok((n, src))) if src.ip() == addr => {
            serde_json::from_slice::<Message>(&buf[..n]).is_ok_and(|m| m == Message::Released)
        }
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_by_type() {
        assert_eq!(
            serde_json::to_string(&Message::Release).unwrap(),
            r#"{"type":"release"}"#
        );
        assert_eq!(
            serde_json::from_str::<Message>(r#"{"type":"released"}"#).unwrap(),
            Message::Released
        );
    }

    #[tokio::test]
    async fn release_reports_silence_as_false() {
        let t = std::time::Instant::now();
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            release("127.0.0.1".parse().unwrap()),
        )
        .await
        .unwrap();
        assert!(matches!(r, Ok(false)), "{r:?}");
        assert!(t.elapsed() < Duration::from_secs(4));
    }
}
