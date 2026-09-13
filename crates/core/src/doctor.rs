//! `hither doctor`: find out, in a few seconds, whether this machine can make
//! direct connections, whether a relay is reachable, and how the world sees
//! it. The point is to turn "it's slow" or "it can't connect" into a sentence
//! the person can act on, or at least understand.
//!
//! Two sources of truth:
//! - a bare UDP self-test to this machine's own LAN address, which is the
//!   check that catches zero-trust clients (Zscaler) dropping UDP; and
//! - iroh's own network report, which adds NAT behaviour, the public address,
//!   captive-portal detection and relay latencies. That API is marked
//!   unstable upstream, so everything touching it lives in this module.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket},
    time::{Duration, Instant},
};

use anyhow::Result;
use iroh::{Endpoint, TransportAddr, Watcher};
use serde::{Deserialize, Serialize};

use crate::{
    identity::{self, Identity},
    net::{self, NetOptions},
};

/// Options for [`diagnose`].
#[derive(Debug, Clone)]
pub struct DoctorOptions {
    pub net: NetOptions,
    /// Upper bound for the whole run.
    pub timeout: Duration,
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self {
            net: NetOptions::default(),
            timeout: Duration::from_secs(12),
        }
    }
}

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Ok,
    Failed,
    Skipped,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpCheck {
    /// UDP to 127.0.0.1 round-trips. If this fails something is very wrong.
    pub loopback: Status,
    /// The address this machine uses to reach the internet, if known.
    pub lan_ip: Option<IpAddr>,
    /// UDP to that LAN address round-trips. Zero-trust clients break this.
    pub lan: Status,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayCheck {
    pub status: Status,
    pub url: Option<String>,
    pub millis: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NatCheck {
    pub udp_v4: Option<bool>,
    pub udp_v6: Option<bool>,
    /// `Some(true)` is a symmetric NAT: hole punching rarely succeeds.
    pub symmetric_nat: Option<bool>,
    pub public_v4: Option<String>,
    pub public_v6: Option<String>,
    pub captive_portal: Option<bool>,
    /// Relay URL and round-trip time in milliseconds, fastest first.
    pub relay_latencies: Vec<(String, u128)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Direct connections should work most of the time.
    DirectLikely,
    /// Direct paths will probably fail; transfers will go via relay.
    RelayOnly,
    /// Neither direct nor relay looked reachable.
    Offline,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    pub identity: Option<IdentityInfo>,
    pub udp: UdpCheck,
    pub relay: RelayCheck,
    /// Addresses iroh would put in a ticket right now.
    pub addrs: Vec<String>,
    pub nat: Option<NatCheck>,
    pub verdict: Verdict,
    /// Plain-language reasons behind the verdict, most important first.
    pub reasons: Vec<String>,
    pub elapsed_millis: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityInfo {
    pub endpoint_id: String,
    pub path: Option<String>,
}

/// Run every check and produce a report. Never creates an identity.
pub async fn diagnose(opts: DoctorOptions) -> Result<DoctorReport> {
    let started = Instant::now();
    let deadline = tokio::time::Instant::now() + opts.timeout;

    let identity = existing_identity();
    let udp = tokio::task::spawn_blocking(udp_self_test).await?;

    // Bring up an endpoint the way `hither <paths>` would, and time the relay.
    let endpoint = net::endpoint(&opts.net, vec![]).await?;
    let relay = if !opts.net.relays_enabled() {
        RelayCheck {
            status: Status::Skipped,
            url: None,
            millis: None,
        }
    } else {
        let t0 = Instant::now();
        let online = tokio::time::timeout_at(deadline, endpoint.online())
            .await
            .is_ok();
        let url = endpoint.addr().relay_urls().next().map(|u| u.to_string());
        RelayCheck {
            status: if online && url.is_some() {
                Status::Ok
            } else {
                Status::Failed
            },
            url,
            millis: online.then(|| t0.elapsed().as_millis()),
        }
    };

    let nat = if opts.net.relays_enabled() {
        net_report(&endpoint, deadline).await
    } else {
        None
    };
    let addrs: Vec<String> = endpoint
        .addr()
        .addrs
        .iter()
        .map(|a| match a {
            TransportAddr::Relay(u) => format!("relay {u}"),
            TransportAddr::Ip(s) => format!("ip {s}"),
            other => format!("{other:?}"),
        })
        .collect();
    endpoint.close().await;

    let (verdict, reasons) = judge(&udp, &relay, nat.as_ref());
    Ok(DoctorReport {
        identity,
        udp,
        relay,
        addrs,
        nat,
        verdict,
        reasons,
        elapsed_millis: started.elapsed().as_millis(),
    })
}

fn existing_identity() -> Option<IdentityInfo> {
    if let Ok(Some(id)) = Identity::from_env() {
        return Some(IdentityInfo {
            endpoint_id: id.endpoint_id().to_string(),
            path: None,
        });
    }
    let path = identity::default_path().ok()?;
    if !path.exists() {
        return None;
    }
    let id = Identity::load_or_create(&path).ok()?;
    Some(IdentityInfo {
        endpoint_id: id.endpoint_id().to_string(),
        path: Some(path.display().to_string()),
    })
}

/// Send one datagram from an unbound socket to a socket bound on `ip` and
/// see whether it arrives. Same-host, so only local policy can stop it.
fn udp_round_trip(ip: IpAddr) -> Status {
    let attempt = || -> std::io::Result<bool> {
        let server = UdpSocket::bind(SocketAddr::new(ip, 0))?;
        server.set_read_timeout(Some(Duration::from_millis(1500)))?;
        let target = server.local_addr()?;
        let client = UdpSocket::bind(SocketAddr::new(
            if ip.is_ipv4() {
                IpAddr::V4(Ipv4Addr::UNSPECIFIED)
            } else {
                IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
            },
            0,
        ))?;
        client.send_to(b"hither", target)?;
        let mut buf = [0u8; 16];
        match server.recv_from(&mut buf) {
            Ok((n, _)) => Ok(&buf[..n] == b"hither"),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e),
        }
    };
    match attempt() {
        Ok(true) => Status::Ok,
        Ok(false) => Status::Failed,
        Err(_) => Status::Unknown,
    }
}

/// The address the OS would use to reach the internet. Connecting a UDP
/// socket sends nothing; it only selects a route.
fn outbound_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("1.1.1.1:53").ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

fn udp_self_test() -> UdpCheck {
    let loopback = udp_round_trip(IpAddr::V4(Ipv4Addr::LOCALHOST));
    let lan_ip = outbound_ip();
    let lan = match lan_ip {
        Some(ip) => udp_round_trip(ip),
        None => Status::Skipped,
    };
    UdpCheck {
        loopback,
        lan_ip,
        lan,
    }
}

async fn net_report(endpoint: &Endpoint, deadline: tokio::time::Instant) -> Option<NatCheck> {
    let mut watcher = endpoint.net_report();
    let report = tokio::time::timeout_at(deadline, async {
        loop {
            if let Some(r) = watcher.get() {
                return Some(r);
            }
            if watcher.updated().await.is_err() {
                return None;
            }
        }
    })
    .await
    .ok()
    .flatten()?;
    let mut relay_latencies: Vec<(String, u128)> = report
        .relay_latency
        .iter()
        .map(|(_probe, url, d)| (url.to_string(), d.as_millis()))
        .collect();
    relay_latencies.sort_by_key(|(_, ms)| *ms);
    relay_latencies.dedup_by(|a, b| a.0 == b.0);
    Some(NatCheck {
        udp_v4: Some(report.udp_v4),
        udp_v6: Some(report.udp_v6),
        symmetric_nat: report.mapping_varies_by_dest(),
        public_v4: report.global_v4.map(|a| a.to_string()),
        public_v6: report.global_v6.map(|a| a.to_string()),
        captive_portal: report.captive_portal,
        relay_latencies,
    })
}

fn judge(udp: &UdpCheck, relay: &RelayCheck, nat: Option<&NatCheck>) -> (Verdict, Vec<String>) {
    let mut reasons = Vec::new();
    let mut direct_ok = true;

    if udp.loopback == Status::Failed {
        direct_ok = false;
        reasons.push(
            "UDP does not even work on loopback; a firewall is blocking this program.".into(),
        );
    }
    match udp.lan {
        Status::Failed => {
            direct_ok = false;
            reasons.push(format!(
                "UDP to this machine's own address ({}) is blocked, so direct (QUIC) connections cannot form.",
                udp.lan_ip.map(|i| i.to_string()).unwrap_or_default()
            ));
            reasons.push(
                "If macOS just asked whether hither may accept incoming network connections, click Allow and run `hither doctor` again. If no dialog appeared, a VPN or zero-trust client (Zscaler, for example) is dropping UDP and only the relay path can work."
                    .into(),
            );
        }
        Status::Skipped => reasons
            .push("No route to the internet was found, so the LAN UDP check was skipped.".into()),
        _ => {}
    }
    if let Some(nat) = nat {
        if nat.udp_v4 == Some(false) && nat.udp_v6 == Some(false) {
            direct_ok = false;
            reasons.push("iroh's probes could not send UDP to any relay.".into());
        }
        if nat.symmetric_nat == Some(true) {
            reasons.push("This network's NAT gives a different public port per destination (symmetric NAT). Hole punching will often fail here.".into());
        }
        if nat.captive_portal == Some(true) {
            reasons.push("A captive portal was detected; sign in to the network first.".into());
        }
    } else if relay.status != Status::Skipped {
        reasons
            .push("iroh's network report did not arrive in time; NAT details are unknown.".into());
    }
    match relay.status {
        Status::Failed => reasons.push("No relay could be reached. Without a relay, hole punching cannot be coordinated and nothing falls back.".into()),
        Status::Skipped => reasons.push("Relays are disabled, so only same-network or public-address peers can connect.".into()),
        _ => {}
    }

    let verdict = match (direct_ok, relay.status) {
        (false, Status::Failed | Status::Skipped) => Verdict::Offline,
        (false, _) => Verdict::RelayOnly,
        (true, _) => {
            if reasons.is_empty() {
                reasons.push("UDP works and a relay answered. Direct connections should form most of the time; the relay covers the rest.".into());
            }
            Verdict::DirectLikely
        }
    };
    (verdict, reasons)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_udp_round_trips() {
        assert_eq!(udp_round_trip(IpAddr::V4(Ipv4Addr::LOCALHOST)), Status::Ok);
    }

    #[test]
    fn blocked_lan_udp_means_relay_only() {
        let udp = UdpCheck {
            loopback: Status::Ok,
            lan_ip: Some("192.168.1.2".parse().unwrap()),
            lan: Status::Failed,
        };
        let relay = RelayCheck {
            status: Status::Ok,
            url: Some("https://relay.example/".into()),
            millis: Some(300),
        };
        let (verdict, reasons) = judge(&udp, &relay, None);
        assert_eq!(verdict, Verdict::RelayOnly);
        assert!(reasons[0].contains("blocked"));
        assert!(reasons[1].contains("click Allow"));
    }
}
