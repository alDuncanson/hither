//! The sending side: hash the selected files, serve them, hand out a ticket.
//!
//! [`Sender::start`] does three things in order: import (hash every file by
//! reference and store a collection that names them), serve (start an
//! endpoint with the blobs protocol behind a router), and mint the ticket.
//! From then on iroh-blobs answers requests on its own; we only translate its
//! provider events into our [`Event`]s so a UI can show who is pulling what.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use futures_buffered::BufferedStreamExt;
use iroh::{Endpoint, EndpointAddr, protocol::Router};
use iroh_blobs::{
    BlobFormat, BlobsProtocol, Hash,
    api::{
        Store, TempTag,
        blobs::{AddPathOptions, AddProgressItem, ImportMode},
    },
    format::collection::Collection,
    provider::events::{
        ConnectMode, EventMask, EventSender as ProviderEventSender, ProviderMessage, RequestMode,
        RequestUpdate,
    },
    store::fs::FsStore,
    ticket::BlobTicket,
};
use n0_future::{FuturesUnordered, StreamExt, task::AbortOnDropHandle};
use tempfile::TempDir;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::{
    code::{Code, CodeServer},
    events::{Event, EventSender, FileEntry, emit},
    link, net,
    net::NetOptions,
    paths::{self, Source},
    throttle::Throttle,
};

// ---------------------------------------------------------------- options

/// How much reachability information to pack into the ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TicketKind {
    /// Endpoint id, relay URL and direct addresses. Longest, most robust.
    #[default]
    Full,
    /// Endpoint id only. Receivers look the sender up through discovery.
    /// Shortest; needs the discovery service to be reachable.
    Short,
}

/// Options for [`Sender::start`].
#[derive(Debug, Clone)]
pub struct SendOptions {
    pub net: NetOptions,
    pub ticket_kind: TicketKind,
    /// Base URL to wrap the ticket in, e.g. `https://example.com/`.
    pub link_base: Option<String>,
    /// How many files to hash concurrently.
    pub import_parallelism: usize,
    /// Also mint a four-word spoken code (see [`crate::code`]).
    pub code: bool,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            net: NetOptions::default(),
            ticket_kind: TicketKind::default(),
            link_base: None,
            import_parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
            code: false,
        }
    }
}

// ----------------------------------------------------------------- sender

/// A running share. Drop or [`shutdown`](Self::shutdown) it to stop serving.
pub struct Sender {
    router: Router,
    store: FsStore,
    ticket: BlobTicket,
    link: Option<String>,
    files: Vec<FileEntry>,
    /// The spoken code and its meeting point, when asked for.
    code: Option<(Code, CodeServer)>,
    /// Keeps the collection alive in the store for as long as we serve it.
    _tag: TempTag,
    /// Removed on drop. The store holds hash trees only; files stay in place.
    dir: TempDir,
    /// Forwards iroh-blobs provider events as our events; aborted on drop.
    _events_task: AbortOnDropHandle<()>,
}

impl Sender {
    /// Hash `inputs`, start serving them, and return once the ticket is ready.
    pub async fn start(inputs: &[PathBuf], opts: SendOptions, events: EventSender) -> Result<Self> {
        // 1. Import.
        let sources = paths::collect(inputs)?;
        let files: Vec<FileEntry> = sources
            .iter()
            .map(|s| FileEntry {
                name: s.name.clone(),
                size: s.size,
            })
            .collect();
        let total_bytes: u64 = files.iter().map(|f| f.size).sum();
        emit(
            &events,
            Event::ImportStarted {
                files: files.len() as u64,
                bytes: total_bytes,
            },
        )
        .await;

        let dir = tempfile::Builder::new()
            .prefix("hither-send-")
            .tempdir_in(std::env::temp_dir())
            .context("could not create a temporary directory")?;
        let store = FsStore::load(dir.path())
            .await
            .context("could not open the blob store")?;
        let (tag, collection) = import(&store, &sources, opts.import_parallelism, &events).await?;
        let hash = tag.hash();
        emit(
            &events,
            Event::ImportDone {
                hash: hash.to_string(),
                files: files.len() as u64,
                bytes: total_bytes,
            },
        )
        .await;

        // 2. Serve.
        let names: Arc<HashMap<Hash, String>> = Arc::new(
            collection
                .iter()
                .map(|(name, hash)| (*hash, name.clone()))
                .collect(),
        );
        let (provider_tx, provider_rx) = mpsc::channel(64);
        let blobs = BlobsProtocol::new(
            &store,
            Some(ProviderEventSender::new(
                provider_tx,
                EventMask {
                    connected: ConnectMode::Notify,
                    get: RequestMode::NotifyLog,
                    ..EventMask::DEFAULT
                },
            )),
        );
        let events_task = AbortOnDropHandle::new(n0_future::task::spawn(forward_provider_events(
            provider_rx,
            names,
            events.clone(),
        )));
        let endpoint = net::endpoint(&opts.net, vec![iroh_blobs::ALPN.to_vec()]).await?;
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();
        net::wait_online(router.endpoint(), &opts.net).await;

        // 3. Mint the ticket.
        let mut addr = router.endpoint().addr();
        apply_ticket_kind(&mut addr, opts.ticket_kind);
        let addrs: Vec<String> = addr.addrs.iter().map(|a| format!("{a:?}")).collect();
        let ticket = BlobTicket::new(addr, hash, BlobFormat::HashSeq);
        let link = match &opts.link_base {
            Some(base) => Some(link::to_link(base, &ticket)?),
            None => None,
        };
        // 4. Optionally, a spoken code: a second endpoint under the code's
        //    key that hands out the ticket.
        // A code that fails to start must not take the share down with it.
        let code = if opts.code {
            let code = Code::generate();
            match CodeServer::start(&code, ticket.to_string(), &opts.net).await {
                Ok(server) => Some((code, server)),
                Err(e) => {
                    warn!("no spoken code for this share: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        emit(
            &events,
            Event::Ready {
                ticket: ticket.to_string(),
                link: link.clone(),
                code: code.as_ref().map(|(c, _)| c.to_string()),
                addrs,
            },
        )
        .await;

        Ok(Self {
            router,
            store,
            ticket,
            link,
            files,
            code,
            _tag: tag,
            dir,
            _events_task: events_task,
        })
    }

    /// The spoken code, if one was minted.
    pub fn code(&self) -> Option<&Code> {
        self.code.as_ref().map(|(c, _)| c)
    }

    /// The meeting point's endpoint, if a code was minted. Tests use its
    /// address in place of discovery.
    pub fn code_endpoint(&self) -> Option<&Endpoint> {
        self.code.as_ref().map(|(_, s)| s.endpoint())
    }

    pub fn ticket(&self) -> &BlobTicket {
        &self.ticket
    }

    /// The endpoint serving this share, for callers that want to talk to
    /// other peers over it (an inbox announce, for example).
    pub fn endpoint(&self) -> &Endpoint {
        self.router.endpoint()
    }

    pub fn link(&self) -> Option<&str> {
        self.link.as_deref()
    }

    pub fn files(&self) -> &[FileEntry] {
        &self.files
    }

    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    /// Stop serving and clean up the temporary store.
    pub async fn shutdown(self) -> Result<()> {
        let Self {
            router,
            store,
            dir,
            code,
            _tag,
            _events_task,
            ..
        } = self;
        drop(_tag);
        if let Some((_, server)) = code {
            server.shutdown().await;
        }
        let _ = tokio::time::timeout(Duration::from_secs(3), router.shutdown()).await;
        store.shutdown().await.ok();
        dir.close().ok();
        Ok(())
    }
}

fn apply_ticket_kind(addr: &mut EndpointAddr, kind: TicketKind) {
    match kind {
        TicketKind::Full => {}
        TicketKind::Short => addr.addrs.clear(),
    }
}

// ----------------------------------------------------------------- import

/// Hash every source and store a collection pointing at them.
async fn import(
    store: &Store,
    sources: &[Source],
    parallelism: usize,
    events: &EventSender,
) -> Result<(TempTag, Collection)> {
    let results: Vec<Result<(String, TempTag, u64)>> =
        n0_future::stream::iter(sources.iter().cloned())
            .map(|src| {
                let store = store.clone();
                let events = events.clone();
                async move { import_one(&store, src, &events).await }
            })
            .buffered_unordered(parallelism.max(1))
            .collect()
            .await;
    let mut items = results.into_iter().collect::<Result<Vec<_>>>()?;
    items.sort_by(|a, b| a.0.cmp(&b.0));
    let (collection, tags): (Collection, Vec<TempTag>) = items
        .into_iter()
        .map(|(name, tag, _)| ((name, tag.hash()), tag))
        .unzip();
    let tag = collection.clone().store(store).await?;
    // The collection now protects the individual blobs from garbage collection.
    drop(tags);
    Ok((tag, collection))
}

/// Hash one file. `TryReference` records the hash tree and points at the
/// original file instead of copying it into the store.
async fn import_one(
    store: &Store,
    src: Source,
    events: &EventSender,
) -> Result<(String, TempTag, u64)> {
    emit(
        events,
        Event::ImportFileStarted {
            name: src.name.clone(),
            size: src.size,
        },
    )
    .await;
    let mut stream = store
        .add_path_with_opts(AddPathOptions {
            path: src.path.clone(),
            mode: ImportMode::TryReference,
            format: BlobFormat::Raw,
        })
        .stream()
        .await;
    let mut throttle = Throttle::default();
    let tag = loop {
        let item = stream
            .next()
            .await
            .with_context(|| format!("hashing {} stopped early", src.path.display()))?;
        match item {
            AddProgressItem::Size(_) | AddProgressItem::CopyDone => {}
            AddProgressItem::CopyProgress(offset) | AddProgressItem::OutboardProgress(offset) => {
                if throttle.ready() {
                    emit(
                        events,
                        Event::ImportFileProgress {
                            name: src.name.clone(),
                            offset,
                        },
                    )
                    .await;
                }
            }
            AddProgressItem::Error(cause) => {
                bail!("could not read {}: {cause}", src.path.display())
            }
            AddProgressItem::Done(tag) => break tag,
        }
    };
    emit(
        events,
        Event::ImportFileDone {
            name: src.name.clone(),
        },
    )
    .await;
    Ok((src.name, tag, src.size))
}

// -------------------------------------------------------- provider events

/// Translate iroh-blobs provider events into our UI-neutral events. One
/// task per connection request keeps per-request progress separate.
async fn forward_provider_events(
    mut rx: mpsc::Receiver<ProviderMessage>,
    names: Arc<HashMap<Hash, String>>,
    events: EventSender,
) {
    let mut tasks = FuturesUnordered::new();
    // Payload bytes sent per connection, so a disconnect can say whether
    // that peer got everything.
    let sent: Arc<Mutex<HashMap<u64, u64>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        tokio::select! {
            biased;
            msg = rx.recv() => {
                let Some(msg) = msg else { break };
                match msg {
                    ProviderMessage::ClientConnectedNotify(m) => {
                        emit(&events, Event::PeerConnected {
                            connection: m.connection_id,
                            peer: m.endpoint_id.map(|id| id.fmt_short().to_string()),
                        }).await;
                    }
                    ProviderMessage::ConnectionClosed(m) => {
                        let bytes_sent = sent.lock().unwrap().remove(&m.connection_id).unwrap_or(0);
                        emit(&events, Event::PeerDisconnected { connection: m.connection_id, bytes_sent }).await;
                    }
                    ProviderMessage::GetRequestReceivedNotify(m) => {
                        let connection = m.connection_id;
                        let request = m.request_id;
                        tasks.push(forward_request(connection, request, m.rx, names.clone(), events.clone(), sent.clone()));
                    }
                    other => debug!("ignoring provider event {other:?}"),
                }
            }
            Some(()) = tasks.next(), if !tasks.is_empty() => {}
        }
    }
    while tasks.next().await.is_some() {}
}

async fn forward_request(
    connection: u64,
    request: u64,
    mut rx: irpc::channel::mpsc::Receiver<RequestUpdate>,
    names: Arc<HashMap<Hash, String>>,
    events: EventSender,
    sent: Arc<Mutex<HashMap<u64, u64>>>,
) {
    let mut throttle = Throttle::default();
    while let Ok(Some(update)) = rx.recv().await {
        match update {
            RequestUpdate::Started(s) => {
                emit(
                    &events,
                    Event::UploadStarted {
                        connection,
                        request,
                        name: names.get(&s.hash).cloned(),
                        size: s.size,
                    },
                )
                .await;
            }
            RequestUpdate::Progress(p) => {
                if throttle.ready() {
                    emit(
                        &events,
                        Event::UploadProgress {
                            connection,
                            request,
                            offset: p.end_offset,
                        },
                    )
                    .await;
                }
            }
            RequestUpdate::Completed(done) => {
                *sent.lock().unwrap().entry(connection).or_insert(0) +=
                    done.stats.payload_bytes_sent;
                emit(
                    &events,
                    Event::UploadDone {
                        connection,
                        request,
                        bytes: done.stats.payload_bytes_sent,
                    },
                )
                .await;
            }
            RequestUpdate::Aborted(_) => {
                emit(
                    &events,
                    Event::UploadAborted {
                        connection,
                        request,
                    },
                )
                .await;
            }
        }
    }
}
