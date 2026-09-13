//! Receiver-initiated transfers.
//!
//! The person who wants the files runs an inbox: a long-lived endpoint with
//! a stable identity. They hand out one link. Whoever opens it *announces* a
//! share (the same collection ticket `hither <paths>` would print, plus the
//! file list) over a tiny protocol of our own; the inbox shows the offer,
//! and once accepted pulls the files with the ordinary verified download.
//!
//! Why announce-then-pull rather than a push: the receiver decides before a
//! payload byte moves, the blobs protocol needs no access control, and the
//! download path is reused unchanged.
//!
//! Wire format: one bidirectional QUIC stream per offer. Every message is a
//! little-endian `u32` length followed by a postcard-encoded value. The
//! sender writes one [`Announce`]; the inbox answers with [`Reply`]s:
//! `Accepted` or `Declined`, then `Done` or `Failed`.

use std::{
    collections::{BTreeSet, HashMap},
    fmt,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayMode,
    endpoint::{Connection, RecvStream, SendStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_blobs::{BlobFormat, ticket::BlobTicket};
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::{
    events::{Event, EventSender, FileEntry, emit},
    identity::{self, Identity},
    link,
    receive::receive_with,
    send::{SendOptions, Sender},
};

/// ALPN for the announce protocol. Bump the suffix on incompatible changes.
pub const ALPN: &[u8] = b"hither/inbox/0";
/// Largest announce we will read: a file list can be long, but not this long.
const MAX_FRAME: usize = 16 * 1024 * 1024;
/// How long an inbox waits for a person to answer the prompt.
const DECISION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

// ------------------------------------------------------------------ ticket

/// What an inbox link carries: how to reach the inbox, and the token that
/// proves the sender was given the link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxTicket {
    pub addr: EndpointAddr,
    pub token: [u8; 16],
}

#[derive(Serialize, Deserialize)]
enum InboxTicketWire {
    V0 { addr: EndpointAddr, token: [u8; 16] },
}

impl Ticket for InboxTicket {
    const KIND: &'static str = "inbox";

    fn encode_bytes(&self) -> Vec<u8> {
        postcard::to_allocvec(&InboxTicketWire::V0 {
            addr: self.addr.clone(),
            token: self.token,
        })
        .expect("postcard serialization failed")
    }

    fn decode_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
        let InboxTicketWire::V0 { addr, token } = postcard::from_bytes(bytes)?;
        Ok(Self { addr, token })
    }
}

impl fmt::Display for InboxTicket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&Ticket::encode_string(self))
    }
}

impl FromStr for InboxTicket {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ticket::decode_string(s)
    }
}

impl InboxTicket {
    pub fn endpoint_id(&self) -> EndpointId {
        self.addr.id
    }
}

// ---------------------------------------------------------------- messages

#[derive(Debug, Serialize, Deserialize)]
pub struct Announce {
    pub token: [u8; 16],
    /// The collection ticket, as `hither <paths>` prints it.
    pub ticket: String,
    pub files: Vec<FileEntry>,
    pub bytes: u64,
    /// A name the sender chose to show, if any.
    pub label: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Reply {
    Accepted,
    Declined { reason: String },
    Done { files: u64, bytes: u64 },
    Failed { reason: String },
}

async fn write_frame<T: Serialize>(send: &mut SendStream, value: &T) -> Result<()> {
    let bytes = postcard::to_allocvec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "message too large");
    send.write_all(&(bytes.len() as u32).to_le_bytes()).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_frame<T: for<'de> Deserialize<'de>>(recv: &mut RecvStream) -> Result<T> {
    let mut len = [0u8; 4];
    recv.read_exact(&mut len)
        .await
        .context("connection closed")?;
    let len = u32::from_le_bytes(len) as usize;
    ensure!(len <= MAX_FRAME, "message too large");
    let mut buf = vec![0u8; len];
    recv.read_exact(&mut buf)
        .await
        .context("connection closed")?;
    Ok(postcard::from_bytes(&buf)?)
}

// ------------------------------------------------------------------- token

/// Where the inbox token lives: beside the identity file.
pub fn token_path() -> Result<PathBuf> {
    let id = identity::default_path()?;
    Ok(id.with_file_name("inbox-token"))
}

/// Load the token, creating it on first use. `rotate` replaces it, which
/// invalidates every link handed out so far.
pub fn load_or_create_token(rotate: bool) -> Result<[u8; 16]> {
    let path = token_path()?;
    if !rotate && path.exists() {
        let text = std::fs::read_to_string(&path)?;
        let bytes = data_encoding::HEXLOWER
            .decode(text.trim().as_bytes())
            .with_context(|| format!("{} is not a valid token", path.display()))?;
        return bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("{} has the wrong length", path.display()));
    }
    use rand::RngExt;
    let token: [u8; 16] = rand::rng().random();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &path,
        format!("{}\n", data_encoding::HEXLOWER.encode(&token)),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(token)
}

// ------------------------------------------------------------------- inbox

/// Who gets in without being asked.
#[derive(Debug, Clone)]
pub enum AcceptPolicy {
    /// Emit an [`Event::Offer`] and wait for [`Inbox::decide`].
    Ask,
    /// Accept everything that carries the right token.
    AcceptAll,
    /// Accept these endpoint ids without asking; ask for everyone else.
    AcceptFrom(BTreeSet<EndpointId>),
}

#[derive(Debug, Clone)]
pub struct InboxOptions {
    /// Offers land in `dir/<label or id>-<timestamp>/`.
    pub dir: PathBuf,
    pub relay: RelayMode,
    pub policy: AcceptPolicy,
    pub link_base: Option<String>,
}

/// A running inbox. Hand out [`Inbox::ticket`] or [`Inbox::link`].
pub struct Inbox {
    router: Router,
    ticket: InboxTicket,
    link: Option<String>,
    handler: InboxHandler,
}

impl Inbox {
    pub async fn open(
        identity: &Identity,
        token: [u8; 16],
        opts: InboxOptions,
        events: EventSender,
    ) -> Result<Self> {
        tokio::fs::create_dir_all(&opts.dir)
            .await
            .with_context(|| format!("could not create {}", opts.dir.display()))?;
        let dir = opts.dir.canonicalize()?;
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(identity.secret_key().clone())
            .relay_mode(opts.relay.clone())
            .alpns(vec![ALPN.to_vec()])
            .bind()
            .await
            .context("could not start networking")?;
        let handler = InboxHandler {
            inner: Arc::new(Inner {
                endpoint: endpoint.clone(),
                token,
                dir,
                policy: opts.policy.clone(),
                events: events.clone(),
                pending: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
                cancel: CancellationToken::new(),
            }),
        };
        let router = Router::builder(endpoint)
            .accept(ALPN, handler.clone())
            .spawn();
        if !matches!(opts.relay, RelayMode::Disabled) {
            let _ = tokio::time::timeout(Duration::from_secs(30), router.endpoint().online()).await;
        }
        // The inbox link should keep working when addresses change, so it
        // carries the id and relay only; discovery fills in the rest.
        let mut addr = router.endpoint().addr();
        addr.addrs
            .retain(|a| matches!(a, iroh::TransportAddr::Relay(_)));
        let ticket = InboxTicket { addr, token };
        let link = match &opts.link_base {
            Some(base) => Some(link::inbox_to_link(base, &ticket)?),
            None => None,
        };
        emit(
            &events,
            Event::InboxReady {
                ticket: ticket.to_string(),
                link: link.clone(),
                endpoint_id: identity.endpoint_id().to_string(),
            },
        )
        .await;
        Ok(Self {
            router,
            ticket,
            link,
            handler,
        })
    }

    pub fn ticket(&self) -> &InboxTicket {
        &self.ticket
    }

    pub fn link(&self) -> Option<&str> {
        self.link.as_deref()
    }

    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    /// A handle for answering offers from another task (the UI).
    pub fn decider(&self) -> Decider {
        Decider {
            inner: self.handler.inner.clone(),
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        self.handler.inner.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.router.shutdown()).await;
        Ok(())
    }
}

/// Answers pending offers.
#[derive(Clone)]
pub struct Decider {
    inner: Arc<Inner>,
}

impl Decider {
    /// Resolve an offer. Returns false if it was no longer pending.
    pub fn decide(&self, id: u64, accept: bool) -> bool {
        let tx = self.inner.pending.lock().unwrap().remove(&id);
        match tx {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }
}

#[derive(Debug, Clone)]
struct InboxHandler {
    inner: Arc<Inner>,
}

struct Inner {
    /// The inbox's own endpoint, so pulls reuse its identity, relay
    /// connection and any direct path the announce already punched.
    endpoint: Endpoint,
    token: [u8; 16],
    dir: PathBuf,
    policy: AcceptPolicy,
    events: EventSender,
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_id: AtomicU64,
    cancel: CancellationToken,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner").field("dir", &self.dir).finish()
    }
}

impl ProtocolHandler for InboxHandler {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        self.inner
            .handle(connection)
            .await
            .map_err(|e| AcceptError::from_err(std::io::Error::other(format!("{e:#}"))))
    }
}

impl Inner {
    async fn handle(&self, connection: Connection) -> Result<()> {
        let from = connection.remote_id();
        let (mut send, mut recv) = connection.accept_bi().await?;
        let announce: Announce = read_frame(&mut recv).await?;

        // Wrong token: say so and hang up. No event, so a stranger probing
        // the endpoint does not light up the UI.
        if !bool::from(announce.token.ct_eq(&self.token)) {
            warn!(
                "rejected announce from {} with a bad token",
                from.fmt_short()
            );
            write_frame(
                &mut send,
                &Reply::Declined {
                    reason: "this link is not valid".into(),
                },
            )
            .await
            .ok();
            send.finish().ok();
            return Ok(());
        }
        let ticket =
            BlobTicket::from_str(&announce.ticket).context("announce carried an invalid ticket")?;
        ensure!(
            ticket.hash_and_format().format == BlobFormat::HashSeq,
            "announce did not carry a collection"
        );
        ensure!(
            ticket.addr().id == from,
            "announce ticket points at a different endpoint than the sender"
        );
        let label = announce
            .label
            .as_deref()
            .map(clean_label)
            .filter(|l| !l.is_empty());
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let from_short = from.fmt_short().to_string();

        let accepted = match &self.policy {
            AcceptPolicy::AcceptAll => {
                self.emit_offer(id, &from, &from_short, &label, &announce, false)
                    .await;
                true
            }
            AcceptPolicy::AcceptFrom(set) if set.contains(&from) => {
                self.emit_offer(id, &from, &from_short, &label, &announce, false)
                    .await;
                true
            }
            _ => {
                let (tx, rx) = oneshot::channel();
                self.pending.lock().unwrap().insert(id, tx);
                self.emit_offer(id, &from, &from_short, &label, &announce, true)
                    .await;
                tokio::select! {
                    r = tokio::time::timeout(DECISION_TIMEOUT, rx) => matches!(r, Ok(Ok(true))),
                    _ = self.cancel.cancelled() => false,
                }
            }
        };
        self.pending.lock().unwrap().remove(&id);

        if !accepted {
            let reason = String::new();
            write_frame(
                &mut send,
                &Reply::Declined {
                    reason: reason.clone(),
                },
            )
            .await
            .ok();
            send.finish().ok();
            emit(&self.events, Event::OfferDeclined { id, reason }).await;
            return Ok(());
        }
        write_frame(&mut send, &Reply::Accepted).await?;
        emit(&self.events, Event::OfferAccepted { id }).await;

        let dir = unique_dir(
            &self.dir,
            &label.clone().unwrap_or_else(|| from_short.clone()),
        );
        emit(
            &self.events,
            Event::OfferStarted {
                id,
                dir: dir.clone(),
            },
        )
        .await;
        let result = receive_with(
            &self.endpoint,
            ticket,
            &dir,
            self.events.clone(),
            self.cancel.child_token(),
        )
        .await;
        match result {
            Ok(received) => {
                write_frame(
                    &mut send,
                    &Reply::Done {
                        files: received.files.len() as u64,
                        bytes: received.bytes,
                    },
                )
                .await
                .ok();
                send.finish().ok();
                emit(
                    &self.events,
                    Event::OfferDone {
                        id,
                        files: received.files.len() as u64,
                        bytes: received.bytes,
                        dir: received.dir,
                    },
                )
                .await;
            }
            Err(e) => {
                let reason = format!("{e:#}");
                write_frame(
                    &mut send,
                    &Reply::Failed {
                        reason: reason.clone(),
                    },
                )
                .await
                .ok();
                send.finish().ok();
                emit(&self.events, Event::OfferFailed { id, reason }).await;
            }
        }
        // Give the peer a moment to read the last frame before the
        // connection goes away with the handler.
        tokio::time::sleep(Duration::from_millis(200)).await;
        Ok(())
    }

    async fn emit_offer(
        &self,
        id: u64,
        from: &EndpointId,
        from_short: &str,
        label: &Option<String>,
        announce: &Announce,
        pending: bool,
    ) {
        emit(
            &self.events,
            Event::Offer {
                id,
                from: from.to_string(),
                from_short: from_short.to_string(),
                label: label.clone(),
                files: announce.files.clone(),
                bytes: announce.bytes,
                pending,
            },
        )
        .await;
    }
}

/// Keep a sender-supplied label safe to print and to use in a folder name.
fn clean_label(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect::<String>()
        .trim()
        .to_string()
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "offer".into() } else { out }
}

/// `dir/<slug>-<timestamp>`, with a counter if that somehow exists.
fn unique_dir(base: &Path, name: &str) -> PathBuf {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let stem = format!("{}-{stamp}", slug(name));
    let mut candidate = base.join(&stem);
    let mut n = 2;
    while candidate.exists() {
        candidate = base.join(format!("{stem}-{n}"));
        n += 1;
    }
    candidate
}

// ------------------------------------------------------------ sender side

/// What `hither to` produces.
#[derive(Debug, Clone)]
pub struct Delivered {
    pub files: u64,
    pub bytes: u64,
}

/// Share `paths` and offer them to `inbox`. Returns once the inbox reports
/// it has everything (or declines).
pub async fn send_to(
    inbox: &InboxTicket,
    paths: &[PathBuf],
    label: Option<String>,
    opts: SendOptions,
    events: EventSender,
    cancel: CancellationToken,
) -> Result<Delivered> {
    let sender = Sender::start(paths, opts, events.clone()).await?;
    let outcome = tokio::select! {
        r = announce_and_wait(&sender, inbox, label, &events) => r,
        _ = cancel.cancelled() => Err(anyhow::Error::new(crate::receive::Cancelled)),
    };
    sender.shutdown().await.ok();
    outcome
}

async fn announce_and_wait(
    sender: &Sender,
    inbox: &InboxTicket,
    label: Option<String>,
    events: &EventSender,
) -> Result<Delivered> {
    let connection = sender
        .endpoint()
        .connect(inbox.addr.clone(), ALPN)
        .await
        .context("could not reach the inbox. Is it open?")?;
    let (mut send, mut recv) = connection.open_bi().await?;
    let bytes = sender.total_bytes();
    write_frame(
        &mut send,
        &Announce {
            token: inbox.token,
            ticket: sender.ticket().to_string(),
            files: sender.files().to_vec(),
            bytes,
            label,
        },
    )
    .await?;
    emit(
        events,
        Event::OfferSent {
            to: inbox.endpoint_id().fmt_short().to_string(),
        },
    )
    .await;
    match read_frame::<Reply>(&mut recv).await? {
        Reply::Accepted => emit(events, Event::ToAccepted).await,
        Reply::Declined { reason } => {
            emit(
                events,
                Event::ToDeclined {
                    reason: reason.clone(),
                },
            )
            .await;
            if reason.is_empty() {
                bail!("the inbox declined the offer");
            }
            bail!("the inbox declined the offer: {reason}");
        }
        other => bail!("unexpected reply from the inbox: {other:?}"),
    }
    match read_frame::<Reply>(&mut recv).await? {
        Reply::Done { files, bytes } => {
            emit(events, Event::ToDone { files, bytes }).await;
            debug!("inbox confirmed {files} files, {bytes} bytes");
            Ok(Delivered { files, bytes })
        }
        Reply::Failed { reason } => bail!("the inbox could not finish: {reason}"),
        other => bail!("unexpected reply from the inbox: {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_ticket_round_trips_and_has_a_readable_prefix() {
        let key = iroh::SecretKey::generate();
        let t = InboxTicket {
            addr: EndpointAddr::new(key.public()),
            token: [7u8; 16],
        };
        let s = t.to_string();
        assert!(s.starts_with("inbox"), "{s}");
        assert_eq!(InboxTicket::from_str(&s).unwrap(), t);
        assert!(BlobTicket::from_str(&s).is_err());
    }

    #[test]
    fn slugs_and_labels_are_tame() {
        assert_eq!(slug("Sam's Scans / Roll 12"), "sam-s-scans-roll-12");
        assert_eq!(slug("!!!"), "offer");
        assert_eq!(clean_label("  hi\u{7}there  "), "hithere");
    }
}
