//! The sending side: hash the selected files, serve them, hand out a ticket.

use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};

use anyhow::{Context, Result, bail};
use futures_buffered::BufferedStreamExt;
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets, protocol::Router};
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
use tracing::debug;

use crate::{
    events::{Event, EventSender, FileEntry, emit},
    link,
    paths::{self, Source},
    throttle::Throttle,
};

/// How much reachability information to pack into the ticket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TicketKind {
    /// Endpoint id, relay URL and direct addresses. Longest, most robust.
    #[default]
    Full,
    /// Endpoint id only. Receivers look the sender up through iroh's DNS
    /// discovery. Shortest; needs the discovery service to be reachable.
    Short,
}

/// Options for [`Sender::start`].
#[derive(Debug, Clone)]
pub struct SendOptions {
    pub ticket_kind: TicketKind,
    pub relay: RelayMode,
    /// Base URL to wrap the ticket in, e.g. `https://share.example`.
    pub link_base: Option<String>,
    /// Fixed identity. `None` generates a fresh one per run.
    pub secret_key: Option<SecretKey>,
    /// How many files to hash concurrently.
    pub import_parallelism: usize,
}

impl Default for SendOptions {
    fn default() -> Self {
        Self {
            ticket_kind: TicketKind::default(),
            relay: RelayMode::Default,
            link_base: None,
            secret_key: None,
            import_parallelism: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4),
        }
    }
}

/// A running share. Drop or [`shutdown`](Self::shutdown) it to stop serving.
pub struct Sender {
    router: Router,
    store: FsStore,
    ticket: BlobTicket,
    link: Option<String>,
    files: Vec<FileEntry>,
    // Keeps the collection alive in the store for as long as we serve it.
    _tag: TempTag,
    // Removed on drop; the store only holds hash trees, files stay in place.
    dir: TempDir,
    _events_task: AbortOnDropHandle<()>,
}

impl Sender {
    /// Hash `inputs`, start serving them, and return once the ticket is ready.
    pub async fn start(inputs: &[PathBuf], opts: SendOptions, events: EventSender) -> Result<Self> {
        let sources = paths::collect(inputs)?;
        let total_bytes: u64 = sources.iter().map(|s| s.size).sum();
        let files: Vec<FileEntry> = sources
            .iter()
            .map(|s| FileEntry {
                name: s.name.clone(),
                size: s.size,
            })
            .collect();
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

        let secret_key = opts.secret_key.unwrap_or_else(SecretKey::generate);
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(secret_key)
            .relay_mode(opts.relay.clone())
            .alpns(vec![iroh_blobs::ALPN.to_vec()])
            .bind()
            .await
            .context("could not start networking")?;
        let router = Router::builder(endpoint)
            .accept(iroh_blobs::ALPN, blobs)
            .spawn();

        // Learn our relay and public addresses before minting the ticket.
        // If the relay is slow to answer we still hand out what we have.
        if !matches!(opts.relay, RelayMode::Disabled) {
            let _ = tokio::time::timeout(Duration::from_secs(30), router.endpoint().online()).await;
        }
        let mut addr = router.endpoint().addr();
        apply_ticket_kind(&mut addr, opts.ticket_kind);
        let addrs: Vec<String> = addr.addrs.iter().map(|a| format!("{a:?}")).collect();
        let ticket = BlobTicket::new(addr, hash, BlobFormat::HashSeq);
        let link = match &opts.link_base {
            Some(base) => Some(link::to_link(base, &ticket)?),
            None => None,
        };
        emit(
            &events,
            Event::Ready {
                ticket: ticket.to_string(),
                link: link.clone(),
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
            _tag: tag,
            dir,
            _events_task: events_task,
        })
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
            _tag,
            _events_task,
            ..
        } = self;
        drop(_tag);
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
    // The collection now protects the individual blobs.
    drop(tags);
    Ok((tag, collection))
}

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
    // TryReference: record the hash tree and point at the original file
    // instead of copying it into the store.
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

/// Translate iroh-blobs provider events into our UI-neutral events.
async fn forward_provider_events(
    mut rx: mpsc::Receiver<ProviderMessage>,
    names: Arc<HashMap<Hash, String>>,
    events: EventSender,
) {
    let mut tasks = FuturesUnordered::new();
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
                        emit(&events, Event::PeerDisconnected { connection: m.connection_id }).await;
                    }
                    ProviderMessage::GetRequestReceivedNotify(m) => {
                        let connection = m.connection_id;
                        let request = m.request_id;
                        tasks.push(forward_request(connection, request, m.rx, names.clone(), events.clone()));
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
