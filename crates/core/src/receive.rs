//! The receiving side: connect, learn what is in the share, download it with
//! verification, and write the files into place.
//!
//! A receive runs in five phases, each a function below:
//!
//! 1. [`connect`]: dial the sender named in the ticket.
//! 2. [`fetch_manifest`]: pull the tiny collection index and the size of
//!    every blob, so names and totals are known before real data moves.
//! 3. [`refuse_overwrite`]: never clobber an existing file.
//! 4. [`download_missing`]: ask for exactly the chunks the local store
//!    lacks, verified against the root hash as they arrive.
//! 5. [`export`]: rename the verified files out of the partial store.
//!
//! The partial store sits beside the destination as `.hither-partial-<hash>`
//! so the export is a rename, not a copy, and an interrupted run resumes.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, endpoint::Connection};
use iroh_blobs::{
    BlobFormat, Hash, HashAndFormat,
    api::{
        blobs::{ExportMode, ExportOptions, ExportProgressItem},
        remote::{GetProgressItem, LocalInfo},
    },
    format::collection::Collection,
    get::{Stats, request::get_hash_seq_and_sizes},
    protocol::{ChunkRanges, ChunkRangesSeq, GetRequest},
    store::fs::FsStore,
    ticket::BlobTicket,
};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    events::{Event, EventSender, FileEntry, PathKind, emit},
    net::{self, NetOptions},
    paths,
    throttle::Throttle,
};

/// Largest collection index we are willing to fetch: 32 MiB of hashes,
/// roughly a million files.
const MAX_HASH_SEQ_BYTES: u64 = 32 * 1024 * 1024;

// ---------------------------------------------------------------- options

/// Options for [`receive`].
#[derive(Debug, Clone)]
pub struct ReceiveOptions {
    /// Directory the files are written into. Created if missing.
    pub out_dir: PathBuf,
    pub net: NetOptions,
}

/// What a completed receive produced.
#[derive(Debug, Clone)]
pub struct Received {
    pub files: Vec<FileEntry>,
    pub bytes: u64,
    pub dir: PathBuf,
    pub elapsed: Duration,
    /// How the sender was reached at the end of the transfer.
    pub path_kind: PathKind,
}

/// Returned when the receive was cancelled through its token. Partial data
/// is kept on disk so running again with the same ticket resumes.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Where partial downloads for `hash` live under `out_dir`.
pub fn partial_dir(out_dir: &Path, hash: &Hash) -> PathBuf {
    out_dir.join(format!(".hither-partial-{}", &hash.to_hex()[..16]))
}

// ------------------------------------------------------------ entry points

/// Download the share described by `ticket` into `opts.out_dir` over a
/// fresh endpoint.
pub async fn receive(
    ticket: BlobTicket,
    opts: ReceiveOptions,
    events: EventSender,
    cancel: CancellationToken,
) -> Result<Received> {
    let endpoint = net::endpoint(&opts.net, vec![]).await?;
    let result = receive_with(&endpoint, ticket, &opts.out_dir, events, cancel).await;
    endpoint.close().await;
    result
}

/// Download the share described by `ticket` into `out_dir` over an endpoint
/// the caller owns (the inbox reuses its own). The endpoint is left open.
pub async fn receive_with(
    endpoint: &Endpoint,
    ticket: BlobTicket,
    out_dir: &Path,
    events: EventSender,
    cancel: CancellationToken,
) -> Result<Received> {
    let HashAndFormat { hash, format } = ticket.hash_and_format();
    ensure!(
        format == BlobFormat::HashSeq,
        "this ticket points at a single blob, not a share of files"
    );
    let out_dir = if out_dir.as_os_str().is_empty() {
        std::env::current_dir()?
    } else {
        out_dir.to_path_buf()
    };
    tokio::fs::create_dir_all(&out_dir)
        .await
        .with_context(|| format!("could not create {}", out_dir.display()))?;
    let out_dir = out_dir.canonicalize()?;

    let store_dir = partial_dir(&out_dir, &hash);
    let store = FsStore::load(&store_dir)
        .await
        .context("could not open the download store")?;

    // Bytes verified by an earlier, interrupted run.
    let had_before = store
        .remote()
        .local(HashAndFormat::hash_seq(hash))
        .await
        .map(|l| l.local_bytes())
        .unwrap_or(0);
    let payload_started = AtomicBool::new(false);

    let result = tokio::select! {
        r = run(&ticket, endpoint, &store, &out_dir, &events, had_before, &payload_started) => r,
        _ = cancel.cancelled() => Err(anyhow::Error::new(Cancelled)),
    };
    store.shutdown().await.ok();

    // Keep the partial store only if it holds real progress: either from
    // before this run or because this run got as far as moving payload.
    let worth_keeping =
        result.is_err() && (had_before > 0 || payload_started.load(Ordering::Relaxed));
    if !worth_keeping {
        tokio::fs::remove_dir_all(&store_dir).await.ok();
    }
    result
}

// ------------------------------------------------------------- the phases

/// Everything known about the share before the payload moves.
struct Manifest {
    files: Vec<PlannedFile>,
    /// Sum of the files' sizes: what the person thinks of as "the download".
    payload_bytes: u64,
    /// Payload plus the index blobs: what progress is measured against.
    total_bytes: u64,
}

struct PlannedFile {
    name: String,
    hash: Hash,
    size: u64,
    /// Where it will be written.
    target: PathBuf,
}

impl Manifest {
    fn entries(&self) -> Vec<FileEntry> {
        self.files
            .iter()
            .map(|f| FileEntry {
                name: f.name.clone(),
                size: f.size,
            })
            .collect()
    }
}

/// Orchestrates the five phases. Small on purpose: the interesting work is
/// in the functions it calls.
#[allow(clippy::too_many_arguments)]
async fn run(
    ticket: &BlobTicket,
    endpoint: &Endpoint,
    store: &FsStore,
    out_dir: &Path,
    events: &EventSender,
    had_before: u64,
    payload_started: &AtomicBool,
) -> Result<Received> {
    let started = Instant::now();
    let hash = ticket.hash();
    let content = HashAndFormat::hash_seq(hash);

    let connection = connect(endpoint, ticket, events).await?;
    let path_kind = Arc::new(Mutex::new(PathKind::Unknown));
    let path_watcher = AbortOnDropHandle::new(n0_future::task::spawn(watch_paths(
        connection.clone(),
        events.clone(),
        path_kind.clone(),
    )));

    let manifest = fetch_manifest(store, &connection, hash, out_dir).await?;
    let local = store.remote().local(content).await?;
    emit(
        events,
        Event::ManifestReceived {
            files: manifest.entries(),
            bytes: manifest.payload_bytes,
            have: had_before.min(manifest.total_bytes),
        },
    )
    .await;
    refuse_overwrite(&manifest)?;

    let (bytes_read, seconds) = download_missing(
        store,
        &connection,
        &local,
        manifest.total_bytes,
        events,
        payload_started,
    )
    .await?;
    emit(
        events,
        Event::DownloadDone {
            bytes: bytes_read,
            seconds,
        },
    )
    .await;

    let final_kind = *path_kind.lock().unwrap();
    drop(path_watcher);
    connection.close(0u32.into(), b"done");

    export(store, &manifest, events).await?;

    let received = Received {
        files: manifest.entries(),
        bytes: manifest.payload_bytes,
        dir: out_dir.to_path_buf(),
        elapsed: started.elapsed(),
        path_kind: final_kind,
    };
    emit(
        events,
        Event::Finished {
            files: received.files.len() as u64,
            bytes: received.bytes,
            dir: received.dir.clone(),
        },
    )
    .await;
    Ok(received)
}

/// Phase 1: dial the sender.
async fn connect(
    endpoint: &Endpoint,
    ticket: &BlobTicket,
    events: &EventSender,
) -> Result<Connection> {
    emit(events, Event::Connecting).await;
    let connection = endpoint
        .connect(ticket.addr().clone(), iroh_blobs::ALPN)
        .await
        .context("could not reach the sender. Are they still sharing?")?;
    emit(
        events,
        Event::Connected {
            peer: connection.remote_id().fmt_short().to_string(),
        },
    )
    .await;
    Ok(connection)
}

/// Phase 2: the collection index and its metadata (names) are tiny, so fetch
/// them first, then ask the sender for every blob's size. Nothing large has
/// moved when this returns.
async fn fetch_manifest(
    store: &FsStore,
    connection: &Connection,
    hash: Hash,
    out_dir: &Path,
) -> Result<Manifest> {
    // Root blob (the hash list) plus its first child (the names).
    let index_request = GetRequest::new(
        hash,
        ChunkRangesSeq::from_ranges([ChunkRanges::all(), ChunkRanges::all()]),
    );
    drive_get(store, connection.clone(), index_request, None).await?;
    let collection = Collection::load(hash, store.as_ref())
        .await
        .context("the share's file list is malformed")?;

    let (hash_seq, sizes) = get_hash_seq_and_sizes(connection, &hash, MAX_HASH_SEQ_BYTES, None)
        .await
        .map_err(|e| anyhow::anyhow!("could not read the share's sizes: {e}"))?;
    // `sizes` is indexed by child: 0 is the names blob, 1.. are files. Guard
    // against an implementation that also reports the root.
    let child_sizes: &[u64] = if sizes.len() == hash_seq.len() + 1 {
        &sizes[1..]
    } else {
        &sizes[..]
    };
    ensure!(
        child_sizes.len() == collection.len() + 1,
        "the share's file list does not match its contents"
    );

    let mut files = Vec::with_capacity(collection.len());
    for ((name, hash), size) in collection.iter().zip(child_sizes.iter().skip(1)) {
        files.push(PlannedFile {
            name: name.clone(),
            hash: *hash,
            size: *size,
            target: paths::destination(out_dir, name)?,
        });
    }
    let payload_bytes = files.iter().map(|f| f.size).sum();
    let root_bytes = 32 * hash_seq.len() as u64;
    Ok(Manifest {
        files,
        payload_bytes,
        total_bytes: root_bytes + child_sizes.iter().sum::<u64>(),
    })
}

/// Phase 3: decide about collisions before downloading gigabytes.
fn refuse_overwrite(manifest: &Manifest) -> Result<()> {
    for f in &manifest.files {
        ensure!(
            !f.target.exists(),
            "{} already exists. Move it away or choose another output directory",
            f.target.display()
        );
    }
    Ok(())
}

/// Phase 4: download whatever is still missing, verified on the way in.
/// Returns payload bytes read and the seconds it took; zero if nothing was
/// missing.
async fn download_missing(
    store: &FsStore,
    connection: &Connection,
    local: &LocalInfo,
    total_bytes: u64,
    events: &EventSender,
    payload_started: &AtomicBool,
) -> Result<(u64, f64)> {
    let have = local.local_bytes().min(total_bytes);
    let outcome = if local.is_complete() {
        (0, 0.0)
    } else {
        payload_started.store(true, Ordering::Relaxed);
        let stats = drive_get(
            store,
            connection.clone(),
            local.missing(),
            Some((events, have, total_bytes)),
        )
        .await?;
        (stats.total_bytes_read(), stats.elapsed.as_secs_f64())
    };
    emit(
        events,
        Event::DownloadProgress {
            bytes: total_bytes,
            total: total_bytes,
        },
    )
    .await;
    Ok(outcome)
}

/// Phase 5: move the verified files into place. `TryReference` renames out
/// of the partial store when it can, which is always, since the store lives
/// on the same file system as the destination.
async fn export(store: &FsStore, manifest: &Manifest, events: &EventSender) -> Result<()> {
    for f in &manifest.files {
        if let Some(parent) = f.target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        emit(
            events,
            Event::ExportFileStarted {
                name: f.name.clone(),
                size: f.size,
            },
        )
        .await;
        let mut stream = store
            .export_with_opts(ExportOptions {
                hash: f.hash,
                target: f.target.clone(),
                mode: ExportMode::TryReference,
            })
            .stream()
            .await;
        let mut throttle = Throttle::default();
        while let Some(item) = stream.next().await {
            match item {
                ExportProgressItem::Size(_) => {}
                ExportProgressItem::CopyProgress(offset) => {
                    if throttle.ready() {
                        emit(
                            events,
                            Event::ExportFileProgress {
                                name: f.name.clone(),
                                offset,
                            },
                        )
                        .await;
                    }
                }
                ExportProgressItem::Done => break,
                ExportProgressItem::Error(cause) => {
                    bail!("could not write {}: {cause}", f.target.display())
                }
            }
        }
        emit(
            events,
            Event::ExportFileDone {
                name: f.name.clone(),
            },
        )
        .await;
    }
    Ok(())
}

// ---------------------------------------------------------------- helpers

/// Run one get request to completion, optionally reporting progress as
/// `(events, bytes already present, total)`.
async fn drive_get(
    store: &FsStore,
    connection: Connection,
    request: GetRequest,
    progress: Option<(&EventSender, u64, u64)>,
) -> Result<Stats> {
    let mut stream = store.remote().execute_get(connection, request).stream();
    let mut throttle = Throttle::default();
    loop {
        let item = stream
            .next()
            .await
            .context("the connection closed before the download finished")?;
        match item {
            GetProgressItem::Progress(offset) => {
                if let Some((events, have, total)) = progress
                    && throttle.ready()
                {
                    emit(
                        events,
                        Event::DownloadProgress {
                            bytes: (have + offset).min(total),
                            total,
                        },
                    )
                    .await;
                }
            }
            GetProgressItem::Done(stats) => return Ok(stats),
            GetProgressItem::Error(cause) => bail!("download failed: {cause}"),
        }
    }
}

/// Report whether we are talking to the sender directly or through a relay,
/// once at connect and again whenever the set of paths changes.
async fn watch_paths(connection: Connection, events: EventSender, kind: Arc<Mutex<PathKind>>) {
    let mut last = PathKind::Unknown;
    let mut report = |k: PathKind| {
        if k != last {
            last = k;
            *kind.lock().unwrap() = k;
            Some(k)
        } else {
            None
        }
    };
    if let Some(k) = report(classify(&connection.paths())) {
        emit(&events, Event::PathChanged { kind: k }).await;
    }
    let mut stream = connection.paths_stream();
    while let Some(list) = stream.next().await {
        if let Some(k) = report(classify(&list)) {
            emit(&events, Event::PathChanged { kind: k }).await;
        }
    }
}

fn classify(paths: &iroh::endpoint::PathList<'_>) -> PathKind {
    let mut any_ip = false;
    for path in paths.iter() {
        if path.is_selected() {
            return if path.is_relay() {
                PathKind::Relay
            } else {
                PathKind::Direct
            };
        }
        any_ip |= path.is_ip();
    }
    if any_ip {
        PathKind::Direct
    } else if paths.is_empty() {
        PathKind::Unknown
    } else {
        PathKind::Relay
    }
}
