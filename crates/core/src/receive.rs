//! The receiving side: connect, learn what is in the share, download it with
//! verification, and write the files into place.

use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail, ensure};
use iroh::{Endpoint, RelayMode, SecretKey, endpoint::Connection, endpoint::presets};
use iroh_blobs::{
    BlobFormat, Hash, HashAndFormat,
    api::{
        blobs::{ExportMode, ExportOptions, ExportProgressItem},
        remote::GetProgressItem,
    },
    format::collection::Collection,
    get::request::get_hash_seq_and_sizes,
    protocol::{ChunkRanges, ChunkRangesSeq, GetRequest},
    store::fs::FsStore,
    ticket::BlobTicket,
};
use n0_future::{StreamExt, task::AbortOnDropHandle};
use tokio_util::sync::CancellationToken;

use crate::{
    events::{Event, EventSender, FileEntry, PathKind, emit},
    paths,
    throttle::Throttle,
};

/// Largest collection index we are willing to fetch (32 MiB of hashes, which
/// is roughly a million files).
const MAX_HASH_SEQ_BYTES: u64 = 32 * 1024 * 1024;

/// Options for [`receive`].
#[derive(Debug, Clone)]
pub struct ReceiveOptions {
    /// Directory the files are written into. Created if missing.
    pub out_dir: PathBuf,
    pub relay: RelayMode,
    pub secret_key: Option<SecretKey>,
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

/// Returned when the receive was cancelled through its token. Partial data is
/// kept on disk so running again with the same ticket resumes.
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
    out_dir.join(format!(".share-partial-{}", &hash.to_hex()[..16]))
}

/// Download the share described by `ticket` into `opts.out_dir`.
pub async fn receive(
    ticket: BlobTicket,
    opts: ReceiveOptions,
    events: EventSender,
    cancel: CancellationToken,
) -> Result<Received> {
    let HashAndFormat { hash, format } = ticket.hash_and_format();
    ensure!(
        format == BlobFormat::HashSeq,
        "this ticket points at a single blob, not a share of files"
    );
    let out_dir = if opts.out_dir.as_os_str().is_empty() {
        std::env::current_dir()?
    } else {
        opts.out_dir.clone()
    };
    tokio::fs::create_dir_all(&out_dir)
        .await
        .with_context(|| format!("could not create {}", out_dir.display()))?;
    let out_dir = out_dir.canonicalize()?;

    // The partial store lives next to the destination so the final export is
    // a rename on the same file system instead of a second copy.
    let store_dir = partial_dir(&out_dir, &hash);
    let store = FsStore::load(&store_dir)
        .await
        .context("could not open the download store")?;

    let secret_key = opts.secret_key.unwrap_or_else(SecretKey::generate);
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(secret_key)
        .relay_mode(opts.relay)
        .bind()
        .await
        .context("could not start networking")?;

    // Bytes verified by an earlier, interrupted run.
    let had_before = store
        .remote()
        .local(HashAndFormat::hash_seq(hash))
        .await
        .map(|l| l.local_bytes())
        .unwrap_or(0);
    let payload_started = AtomicBool::new(false);

    let result = tokio::select! {
        r = run(&ticket, &endpoint, &store, &out_dir, &events, had_before, &payload_started) => r,
        _ = cancel.cancelled() => Err(anyhow::Error::new(Cancelled)),
    };
    endpoint.close().await;
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
    let path_kind = Arc::new(Mutex::new(PathKind::Unknown));
    let path_watcher = AbortOnDropHandle::new(n0_future::task::spawn(watch_paths(
        connection.clone(),
        events.clone(),
        path_kind.clone(),
    )));

    // 1. The collection index and its metadata (names) are tiny; fetch them
    //    first so we can show the file list and check for collisions before
    //    moving any real data.
    let manifest = GetRequest::new(
        hash,
        ChunkRangesSeq::from_ranges([ChunkRanges::all(), ChunkRanges::all()]),
    );
    drive_get(store, connection.clone(), manifest, None).await?;
    let collection = Collection::load(hash, store.as_ref())
        .await
        .context("the share's file list is malformed")?;

    // 2. Sizes of every blob, verified against the sender.
    let (hash_seq, sizes) = get_hash_seq_and_sizes(&connection, &hash, MAX_HASH_SEQ_BYTES, None)
        .await
        .map_err(|e| anyhow::anyhow!("could not read the share's sizes: {e}"))?;
    // `sizes` is indexed by child (0 = metadata blob, 1.. = files). Guard
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
    let files: Vec<(String, Hash, u64)> = collection
        .iter()
        .zip(child_sizes.iter().skip(1))
        .map(|((name, hash), size)| (name.clone(), *hash, *size))
        .collect();
    let payload_bytes: u64 = files.iter().map(|(_, _, s)| *s).sum();
    let root_bytes = 32 * hash_seq.len() as u64;
    let total_bytes = root_bytes + child_sizes.iter().sum::<u64>();

    let local = store.remote().local(content).await?;
    // Base for cumulative progress; includes the index we just fetched.
    let have = local.local_bytes().min(total_bytes);
    emit(
        events,
        Event::ManifestReceived {
            files: files
                .iter()
                .map(|(name, _, size)| FileEntry {
                    name: name.clone(),
                    size: *size,
                })
                .collect(),
            bytes: payload_bytes,
            have: had_before.min(total_bytes),
        },
    )
    .await;

    // 3. Never overwrite. Decide this before downloading gigabytes.
    let mut targets = Vec::with_capacity(files.len());
    for (name, _, _) in &files {
        let target = paths::destination(out_dir, name)?;
        ensure!(
            !target.exists(),
            "{} already exists. Move it away or choose another output directory",
            target.display()
        );
        targets.push(target);
    }

    // 4. Download whatever is still missing, with BLAKE3 verification on the
    //    way in. Progress is reported as cumulative bytes including what an
    //    earlier interrupted run already stored.
    if local.is_complete() {
        emit(
            events,
            Event::DownloadProgress {
                bytes: total_bytes,
                total: total_bytes,
            },
        )
        .await;
        emit(
            events,
            Event::DownloadDone {
                bytes: 0,
                seconds: 0.0,
            },
        )
        .await;
    } else {
        payload_started.store(true, Ordering::Relaxed);
        let stats = drive_get(
            store,
            connection.clone(),
            local.missing(),
            Some((events, have, total_bytes)),
        )
        .await?;
        emit(
            events,
            Event::DownloadProgress {
                bytes: total_bytes,
                total: total_bytes,
            },
        )
        .await;
        emit(
            events,
            Event::DownloadDone {
                bytes: stats.total_bytes_read(),
                seconds: stats.elapsed.as_secs_f64(),
            },
        )
        .await;
    }
    let final_kind = *path_kind.lock().unwrap();
    drop(path_watcher);
    connection.close(0u32.into(), b"done");

    // 5. Move the verified files into place.
    for ((name, hash, size), target) in files.iter().zip(targets) {
        if let Some(parent) = target.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        emit(
            events,
            Event::ExportFileStarted {
                name: name.clone(),
                size: *size,
            },
        )
        .await;
        let mut stream = store
            .export_with_opts(ExportOptions {
                hash: *hash,
                target: target.clone(),
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
                                name: name.clone(),
                                offset,
                            },
                        )
                        .await;
                    }
                }
                ExportProgressItem::Done => break,
                ExportProgressItem::Error(cause) => {
                    bail!("could not write {}: {cause}", target.display())
                }
            }
        }
        emit(events, Event::ExportFileDone { name: name.clone() }).await;
    }

    let received = Received {
        files: files
            .iter()
            .map(|(name, _, size)| FileEntry {
                name: name.clone(),
                size: *size,
            })
            .collect(),
        bytes: payload_bytes,
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

/// Run one get request to completion, optionally reporting progress as
/// `(events, already_have, total)`.
async fn drive_get(
    store: &FsStore,
    connection: Connection,
    request: GetRequest,
    progress: Option<(&EventSender, u64, u64)>,
) -> Result<iroh_blobs::get::Stats> {
    let mut stream = store.remote().execute_get(connection, request).stream();
    let mut throttle = Throttle::default();
    loop {
        let item = stream
            .next()
            .await
            .context("the connection closed before the download finished")?;
        match item {
            GetProgressItem::Progress(offset) => {
                if let Some((events, have, total)) = progress {
                    if throttle.ready() {
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
            }
            GetProgressItem::Done(stats) => return Ok(stats),
            GetProgressItem::Error(cause) => bail!("download failed: {cause}"),
        }
    }
}

/// Report whether we are talking to the sender directly or through a relay.
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
