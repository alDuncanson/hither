//! Receiver-initiated transfers.
//!
//! The person who wants the files runs an inbox: a long-lived endpoint under
//! their persistent identity. They hand out one link. Whoever opens it
//! *announces* a share (the same collection ticket `hither <paths>` would
//! print, plus the file list) over a small protocol of our own. The inbox
//! shows the offer and, once accepted, pulls the files with the ordinary
//! verified download.
//!
//! Why announce-then-pull rather than a push: the receiver decides before a
//! payload byte moves, the blobs protocol needs no access control, and the
//! download path is reused unchanged.
//!
//! Wire format: one bidirectional QUIC stream per offer. Every message is a
//! little-endian `u32` length followed by a postcard-encoded value. The
//! sender writes one [`Announce`]; the inbox answers with [`Reply`]s:
//! `Accepted` or `Declined`, then `Done` or `Failed`.
//!
//! Layout of this file: the ticket, the wire messages, the token, the inbox
//! (open / decide / shutdown), the protocol handler that serves one offer,
//! the sender side (`send_to`), helpers, tests.

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
    Endpoint, EndpointAddr, EndpointId, TransportAddr,
    endpoint::{Connection, RecvStream, SendStream},
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
    net::{self, NetOptions},
    receive::{Received, receive_with},
    send::{SendOptions, Sender},
};

/// ALPN for the announce protocol. Bump the suffix on incompatible changes so
/// old and new versions fail cleanly instead of misparsing each other.
pub const ALPN: &[u8] = b"hither/inbox/0";
/// Largest message we will read. A file list can be long, but not this long.
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

/// Versioned wire form, so the encoding can change without breaking old links.
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

/// Sent once by the offering side.
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

/// Sent by the inbox: a decision, then an outcome.
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
    Ok(identity::default_path()?.with_file_name("inbox-token"))
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
    let token = random_token();
    save_token(token)?;
    Ok(token)
}

/// Write a token (for example one restored from another machine).
pub fn save_token(token: [u8; 16]) -> Result<()> {
    let path = token_path()?;
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
    Ok(())
}

/// Sixteen random bytes.
pub fn random_token() -> [u8; 16] {
    use rand::RngExt;
    rand::rng().random()
}

// ------------------------------------------------------------------- inbox

/// Who gets in without being asked.
#[derive(Debug, Clone)]
pub enum AcceptPolicy {
    /// Emit an [`Event::Offer`] with `pending: true` and wait for
    /// [`Decider::decide`].
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
    pub net: NetOptions,
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
    /// Start listening under `identity`. The identity's key overrides any key
    /// in `opts.net`: an inbox is only useful if its address is stable.
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

        let net = opts
            .net
            .clone()
            .with_secret_key(identity.secret_key().clone());
        let endpoint = net::endpoint(&net, vec![ALPN.to_vec()]).await?;
        let handler = InboxHandler {
            inner: Arc::new(Inner {
                endpoint: endpoint.clone(),
                token,
                dir,
                policy: opts.policy.clone(),
                events: events.clone(),
                pending: Arc::new(Mutex::new(HashMap::new())),
                next_id: AtomicU64::new(1),
                cancel: CancellationToken::new(),
            }),
        };
        let router = Router::builder(endpoint)
            .accept(ALPN, handler.clone())
            .spawn();
        net::wait_online(router.endpoint(), &net).await;

        let ticket = InboxTicket {
            addr: stable_addr(router.endpoint().addr()),
            token,
        };
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
            pending: self.handler.inner.pending.clone(),
        }
    }

    pub async fn shutdown(self) -> Result<()> {
        self.handler.inner.cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(3), self.router.shutdown()).await;
        Ok(())
    }
}

/// An inbox link should keep working when the machine changes networks, so
/// it carries the relay only and lets discovery fill in direct addresses.
/// Without a relay (tests, LAN-only use) there is nothing else to carry, so
/// keep the direct addresses.
fn stable_addr(mut addr: EndpointAddr) -> EndpointAddr {
    let has_relay = addr
        .addrs
        .iter()
        .any(|a| matches!(a, TransportAddr::Relay(_)));
    if has_relay {
        addr.addrs.retain(|a| matches!(a, TransportAddr::Relay(_)));
    }
    addr
}

/// Open questions, by offer id. Shared between the handler (which asks) and
/// the [`Decider`] (which answers).
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<bool>>>>;

/// Answers pending offers. Holds only the table of open questions, never the
/// inbox itself, so a UI task keeping a `Decider` cannot keep the inbox or
/// its event channel alive after shutdown.
#[derive(Clone)]
pub struct Decider {
    pending: Pending,
}

impl Decider {
    /// Resolve an offer. Returns false if it was no longer pending.
    pub fn decide(&self, id: u64, accept: bool) -> bool {
        let tx = self.pending.lock().unwrap().remove(&id);
        match tx {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }
}

// ------------------------------------------------------- protocol handler

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
    pending: Pending,
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

/// One incoming offer, after the announce passed the token and ticket checks.
struct Offer {
    id: u64,
    from: EndpointId,
    from_short: String,
    label: Option<String>,
    ticket: BlobTicket,
    files: Vec<FileEntry>,
    bytes: u64,
}

impl Inner {
    /// Serve one connection, which is one offer: authorize, decide, pull,
    /// report. Each step is its own method below.
    async fn handle(&self, connection: Connection) -> Result<()> {
        let from = connection.remote_id();
        let (mut send, mut recv) = connection.accept_bi().await?;
        let announce: Announce = read_frame(&mut recv).await?;

        let Some(offer) = self.authorize(from, announce, &mut send).await? else {
            return finish(send).await;
        };
        if !self.decide(&offer).await {
            write_frame(
                &mut send,
                &Reply::Declined {
                    reason: String::new(),
                },
            )
            .await
            .ok();
            emit(
                &self.events,
                Event::OfferDeclined {
                    id: offer.id,
                    reason: String::new(),
                },
            )
            .await;
            return finish(send).await;
        }
        write_frame(&mut send, &Reply::Accepted).await?;
        emit(&self.events, Event::OfferAccepted { id: offer.id }).await;

        let result = self.pull(&offer).await;
        self.report(&mut send, offer.id, result).await;
        finish(send).await
    }

    /// Check the token and the ticket. `Ok(None)` means we hung up on a
    /// stranger: no event, so a probe never lights up the UI.
    async fn authorize(
        &self,
        from: EndpointId,
        announce: Announce,
        send: &mut SendStream,
    ) -> Result<Option<Offer>> {
        if !bool::from(announce.token.ct_eq(&self.token)) {
            warn!(
                "rejected announce from {} with a bad token",
                from.fmt_short()
            );
            write_frame(
                send,
                &Reply::Declined {
                    reason: "this link is not valid".into(),
                },
            )
            .await
            .ok();
            return Ok(None);
        }
        let ticket =
            BlobTicket::from_str(&announce.ticket).context("announce carried an invalid ticket")?;
        ensure!(
            ticket.hash_and_format().format == BlobFormat::HashSeq,
            "announce did not carry a collection"
        );
        // The pull goes to whoever the ticket names. Insist that it is the
        // peer talking to us, so nobody can point our inbox at a third party.
        ensure!(
            ticket.addr().id == from,
            "announce ticket points at a different endpoint than the sender"
        );
        Ok(Some(Offer {
            id: self.next_id.fetch_add(1, Ordering::Relaxed),
            from,
            from_short: from.fmt_short().to_string(),
            label: announce
                .label
                .as_deref()
                .map(clean_label)
                .filter(|l| !l.is_empty()),
            ticket,
            files: announce.files,
            bytes: announce.bytes,
        }))
    }

    /// Apply the policy. For `Ask`, emit the offer and wait for the answer.
    async fn decide(&self, offer: &Offer) -> bool {
        let auto = match &self.policy {
            AcceptPolicy::AcceptAll => true,
            AcceptPolicy::AcceptFrom(set) => set.contains(&offer.from),
            AcceptPolicy::Ask => false,
        };
        if auto {
            self.emit_offer(offer, false).await;
            return true;
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(offer.id, tx);
        self.emit_offer(offer, true).await;
        let accepted = tokio::select! {
            r = tokio::time::timeout(DECISION_TIMEOUT, rx) => matches!(r, Ok(Ok(true))),
            _ = self.cancel.cancelled() => false,
        };
        self.pending.lock().unwrap().remove(&offer.id);
        accepted
    }

    /// Download the accepted share into a fresh folder under the inbox dir.
    async fn pull(&self, offer: &Offer) -> Result<Received> {
        let folder = offer
            .label
            .clone()
            .unwrap_or_else(|| offer.from_short.clone());
        let dir = unique_dir(&self.dir, &folder);
        emit(
            &self.events,
            Event::OfferStarted {
                id: offer.id,
                dir: dir.clone(),
            },
        )
        .await;
        receive_with(
            &self.endpoint,
            offer.ticket.clone(),
            &dir,
            self.events.clone(),
            self.cancel.child_token(),
        )
        .await
    }

    /// Tell the sender how it ended, and the UI too.
    async fn report(&self, send: &mut SendStream, id: u64, result: Result<Received>) {
        match result {
            Ok(received) => {
                let files = received.files.len() as u64;
                write_frame(
                    send,
                    &Reply::Done {
                        files,
                        bytes: received.bytes,
                    },
                )
                .await
                .ok();
                emit(
                    &self.events,
                    Event::OfferDone {
                        id,
                        files,
                        bytes: received.bytes,
                        dir: received.dir,
                    },
                )
                .await;
            }
            Err(e) => {
                let reason = format!("{e:#}");
                write_frame(
                    send,
                    &Reply::Failed {
                        reason: reason.clone(),
                    },
                )
                .await
                .ok();
                emit(&self.events, Event::OfferFailed { id, reason }).await;
            }
        }
    }

    async fn emit_offer(&self, offer: &Offer, pending: bool) {
        emit(
            &self.events,
            Event::Offer {
                id: offer.id,
                from: offer.from.to_string(),
                from_short: offer.from_short.clone(),
                label: offer.label.clone(),
                files: offer.files.clone(),
                bytes: offer.bytes,
                pending,
            },
        )
        .await;
    }
}

// ------------------------------------------------------------ sender side

/// What `hither to` produces.
#[derive(Debug, Clone)]
pub struct Delivered {
    pub files: u64,
    pub bytes: u64,
}

/// Share `paths` and offer them to `inbox`. Returns once the inbox reports it
/// has everything, or declines.
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

/// Open the announce stream, send the offer, relay the two replies as events.
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
    write_frame(
        &mut send,
        &Announce {
            token: inbox.token,
            ticket: sender.ticket().to_string(),
            files: sender.files().to_vec(),
            bytes: sender.total_bytes(),
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

    let decision = read_frame::<Reply>(&mut recv)
        .await
        .context("the inbox went away before deciding")?;
    match decision {
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

    let outcome = read_frame::<Reply>(&mut recv)
        .await
        .context("the inbox went away before it had everything")?;
    match outcome {
        Reply::Done { files, bytes } => {
            emit(events, Event::ToDone { files, bytes }).await;
            debug!("inbox confirmed {files} files, {bytes} bytes");
            Ok(Delivered { files, bytes })
        }
        Reply::Failed { reason } => bail!("the inbox could not finish: {reason}"),
        other => bail!("unexpected reply from the inbox: {other:?}"),
    }
}

// ---------------------------------------------------------------- helpers

/// End our side of the announce stream and wait until the peer has read
/// everything (or two seconds, whichever is first). Returning from the
/// handler closes the connection, which would discard an unread reply.
async fn finish(mut send: SendStream) -> Result<()> {
    send.finish().ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), send.stopped()).await;
    Ok(())
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

/// Lowercase ASCII letters and digits, runs of anything else become one `-`.
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
