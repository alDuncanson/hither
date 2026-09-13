//! Spoken codes: four words that stand in for a ticket.
//!
//! A ticket is 140 to 210 characters, fine for a link and hopeless to read
//! over the phone. A code is `able-cactus-river-mouse`: four words from the
//! 2048-word list, 44 bits of entropy.
//!
//! How it works without a server: both sides derive the same keypair from
//! the words. The sharing side runs a second, short-lived endpoint under
//! that key (the *meeting point*) which hands the real ticket to whoever
//! dials it. The receiving side derives the same endpoint id and dials it
//! through iroh's normal discovery. Knowing the words is the only way to
//! compute the id, so knowing the words is the only way to get the ticket.
//!
//! Trade-offs, honestly: anyone can check whether a code is live with one
//! DNS lookup, so codes must be long enough (four words) and short-lived
//! (they die with the share). Two-word codes need a PAKE and a rendezvous
//! server; that is a later option, not this one.

use std::fmt;

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, SecretKey,
    endpoint::Connection,
    protocol::{AcceptError, ProtocolHandler, Router},
};
use tracing::debug;

use crate::{
    net::{self, NetOptions},
    words,
};

/// ALPN for the meeting point. Bump the suffix on incompatible changes.
pub const ALPN: &[u8] = b"hither/code/0";
/// Words per code.
pub const WORDS: usize = 4;
/// Domain separator for key derivation; changing it changes every code's id.
const KEY_CONTEXT: &str = "hither spoken code v0";
/// Largest payload a meeting point will hand over.
const MAX_PAYLOAD: usize = 4096;

// ------------------------------------------------------------------- code

/// Four words. Printed with dashes, parsed from dashes, spaces or commas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Code(Vec<u16>);

impl Code {
    pub fn generate() -> Self {
        use rand::RngExt;
        let mut rng = rand::rng();
        Self((0..WORDS).map(|_| rng.random_range(0..2048u16)).collect())
    }

    /// Exactly four known words, in any of the accepted separators.
    pub fn parse(text: &str) -> Result<Self> {
        let parts = words::split(text);
        ensure!(
            parts.len() == WORDS,
            "a code is {WORDS} words, got {}",
            parts.len()
        );
        let mut idx = Vec::with_capacity(WORDS);
        for w in parts {
            idx.push(words::index_of(w).with_context(|| format!("{w:?} is not a code word"))?);
        }
        Ok(Self(idx))
    }

    /// True if `text` is four words from the list. Used by the CLI to tell a
    /// code apart from paths and tickets.
    pub fn looks_like(text: &str) -> bool {
        Self::parse(text).is_ok()
    }

    /// The keypair both sides derive from the words.
    pub fn secret_key(&self) -> SecretKey {
        let derived = blake3::derive_key(KEY_CONTEXT, self.to_string().as_bytes());
        SecretKey::from_bytes(&derived)
    }

    /// Where the meeting point lives.
    pub fn endpoint_id(&self) -> EndpointId {
        self.secret_key().public()
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let joined = self
            .0
            .iter()
            .map(|&i| words::word(i))
            .collect::<Vec<_>>()
            .join("-");
        f.write_str(&joined)
    }
}

// ---------------------------------------------------------- meeting point

/// A running meeting point: hands `payload` (a ticket string) to anyone who
/// dials the code's endpoint id. Lives until dropped or shut down.
pub struct CodeServer {
    router: Router,
}

impl CodeServer {
    /// Start under the code's key. `net` supplies relay and discovery
    /// settings; its key is replaced by the code's.
    pub async fn start(code: &Code, payload: String, net: &NetOptions) -> Result<Self> {
        ensure!(payload.len() <= MAX_PAYLOAD, "payload too large for a code");
        let net = net.clone().with_secret_key(code.secret_key());
        let endpoint = net::endpoint(&net, vec![ALPN.to_vec()]).await?;
        let router = Router::builder(endpoint)
            .accept(ALPN, Handout { payload })
            .spawn();
        net::wait_online(router.endpoint(), &net).await;
        Ok(Self { router })
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    pub async fn shutdown(self) {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(2), self.router.shutdown()).await;
    }
}

#[derive(Debug, Clone)]
struct Handout {
    payload: String,
}

impl ProtocolHandler for Handout {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        let (mut send, mut recv) = connection.accept_bi().await?;
        // The dialer says hello with one byte so the stream exists on both
        // sides; then we hand over the payload and finish.
        let mut hello = [0u8; 1];
        recv.read_exact(&mut hello)
            .await
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?;
        send.write_all(self.payload.as_bytes())
            .await
            .map_err(|e| AcceptError::from_err(std::io::Error::other(e.to_string())))?;
        send.finish().ok();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), send.stopped()).await;
        debug!(
            "handed out payload to {}",
            connection.remote_id().fmt_short()
        );
        Ok(())
    }
}

/// Dial the code's meeting point and fetch its payload. Discovery can lag a
/// few seconds behind a fresh share, so this retries for a while.
pub async fn redeem(code: &Code, net: &NetOptions) -> Result<String> {
    let endpoint = net::endpoint(net, vec![]).await?;
    let result = redeem_with(&endpoint, code, net).await;
    endpoint.close().await;
    result
}

/// [`redeem`] over an endpoint the caller owns.
pub async fn redeem_with(endpoint: &Endpoint, code: &Code, net: &NetOptions) -> Result<String> {
    // The receiver knows the id and, in tests, the address; otherwise
    // discovery fills the rest in.
    let mut addr = EndpointAddr::new(code.endpoint_id());
    for known in &net.static_peers {
        if known.id == addr.id {
            addr = known.clone();
        }
    }
    let attempts = 5;
    let mut last_err = None;
    for attempt in 1..=attempts {
        match tokio::time::timeout(
            std::time::Duration::from_secs(8),
            endpoint.connect(addr.clone(), ALPN),
        )
        .await
        {
            Ok(Ok(connection)) => {
                let (mut send, mut recv) = connection.open_bi().await?;
                send.write_all(&[1u8]).await?;
                send.finish().ok();
                let bytes = recv
                    .read_to_end(MAX_PAYLOAD)
                    .await
                    .context("the meeting point closed before handing over the ticket")?;
                connection.close(0u32.into(), b"thanks");
                let text = String::from_utf8(bytes).context("the meeting point sent garbage")?;
                ensure!(!text.is_empty(), "the meeting point sent nothing");
                return Ok(text);
            }
            Ok(Err(e)) => last_err = Some(e.to_string()),
            Err(_) => last_err = Some("timed out".into()),
        }
        debug!("code lookup attempt {attempt}/{attempts} failed: {last_err:?}");
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    bail!(
        "nobody is sharing under the code {code} right now ({}). Codes only work while the sender's window is open.",
        last_err.unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_print_parse_and_derive_deterministically() {
        let code = Code::generate();
        let text = code.to_string();
        assert_eq!(text.split('-').count(), 4);
        let again = Code::parse(&text).unwrap();
        assert_eq!(again, code);
        assert_eq!(again.endpoint_id(), code.endpoint_id());
        assert_eq!(Code::parse(&text.replace('-', " ")).unwrap(), code);
        assert!(Code::looks_like(&text));
        assert!(!Code::looks_like("able cactus river"));
        assert!(!Code::looks_like("able cactus river xylophone"));
        assert!(!Code::looks_like("photos/"));
    }

    #[test]
    fn different_codes_give_different_ids() {
        let a = Code::parse("able cactus river mouse").unwrap();
        let b = Code::parse("able cactus river movie").unwrap();
        assert_ne!(a.endpoint_id(), b.endpoint_id());
        // Same words, same id, on every machine.
        assert_eq!(
            a.endpoint_id(),
            Code::parse("able-cactus-river-mouse")
                .unwrap()
                .endpoint_id()
        );
    }
}
