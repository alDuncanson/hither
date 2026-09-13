//! The one place that knows how an iroh endpoint is configured.
//!
//! Every command needs an endpoint: the sender to serve, the receiver to
//! pull, the inbox to listen, the doctor to probe. They differ only in the
//! key they use and whether relays and discovery are on. Those knobs are
//! [`NetOptions`]; everything else is decided here, once.

use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::presets};

/// How long to wait for the endpoint to learn its relay address before
/// carrying on with whatever addresses it already has.
pub const ONLINE_TIMEOUT: Duration = Duration::from_secs(30);

/// Network knobs shared by every command.
#[derive(Debug, Clone)]
pub struct NetOptions {
    /// iroh's default relays, none at all, or a custom relay URL.
    pub relay: RelayMode,
    /// The identity to run under. `None` generates a throwaway key.
    pub secret_key: Option<SecretKey>,
    /// Publish and look up addresses through n0's DNS/pkarr discovery.
    /// On for real use; off in tests, where everything lives on loopback.
    pub discovery: bool,
    /// Bind to one address instead of every interface. Tests pass
    /// `127.0.0.1:0` so two endpoints in one process can reach each other
    /// even on machines where UDP to the LAN address is blocked.
    pub bind: Option<SocketAddr>,
}

impl Default for NetOptions {
    fn default() -> Self {
        Self {
            relay: RelayMode::Default,
            secret_key: None,
            discovery: true,
            bind: None,
        }
    }
}

impl NetOptions {
    /// Loopback only, no relays, no discovery: for tests and same-machine use.
    pub fn local() -> Self {
        Self {
            relay: RelayMode::Disabled,
            secret_key: None,
            discovery: false,
            bind: Some(([127, 0, 0, 1], 0).into()),
        }
    }

    pub fn with_secret_key(mut self, key: SecretKey) -> Self {
        self.secret_key = Some(key);
        self
    }

    pub fn relays_enabled(&self) -> bool {
        !matches!(self.relay, RelayMode::Disabled)
    }
}

/// Build and bind an endpoint. `alpns` lists the protocols this endpoint
/// will accept; a pure client passes an empty list.
pub async fn endpoint(opts: &NetOptions, alpns: Vec<Vec<u8>>) -> Result<Endpoint> {
    let key = opts.secret_key.clone().unwrap_or_else(SecretKey::generate);
    let mut builder = if opts.discovery {
        Endpoint::builder(presets::N0)
    } else {
        Endpoint::builder(presets::Minimal)
    };
    builder = builder
        .secret_key(key)
        .relay_mode(opts.relay.clone())
        .alpns(alpns);
    if let Some(addr) = opts.bind {
        builder = builder
            .bind_addr(addr)
            .with_context(|| format!("cannot bind to {addr}"))?;
    }
    builder.bind().await.context("could not start networking")
}

/// Wait until the endpoint has a relay connection, bounded by
/// [`ONLINE_TIMEOUT`]. A no-op when relays are disabled.
pub async fn wait_online(endpoint: &Endpoint, opts: &NetOptions) {
    if opts.relays_enabled() {
        let _ = tokio::time::timeout(ONLINE_TIMEOUT, endpoint.online()).await;
    }
}
