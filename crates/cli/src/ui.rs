//! Terminal rendering of the core's event stream.

use std::{collections::HashMap, time::Duration};

use console::style;
use hither_core::{Event, EventReceiver, PathKind};
use indicatif::{
    HumanBytes, HumanDuration, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle,
};

const TICK: Duration = Duration::from_millis(120);

fn bytes_bar(mp: &MultiProgress, total: u64, prefix: String) -> ProgressBar {
    let pb = mp.add(ProgressBar::new(total));
    pb.set_style(
        ProgressStyle::with_template(
            "{prefix:>10} [{bar:30.cyan/dim}] {bytes:>9}/{total_bytes:<9} {bytes_per_sec:>10} {msg}",
        )
        .unwrap()
        .progress_chars("━╸─"),
    );
    pb.set_prefix(prefix);
    pb.enable_steady_tick(TICK);
    pb
}

fn spinner(mp: &MultiProgress, msg: &str) -> ProgressBar {
    let pb = mp.add(ProgressBar::new_spinner());
    pb.set_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").unwrap());
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(TICK);
    pb
}

/// Status line to stderr. Works even when stderr is not a terminal, unlike
/// `MultiProgress::println`, which is a no-op for hidden draw targets.
fn say(mp: &MultiProgress, msg: impl std::fmt::Display) {
    mp.suspend(|| eprintln!("{msg}"));
}

/// The one thing scripts want to capture: the ticket block, on stdout.
fn say_out(mp: &MultiProgress, msg: impl std::fmt::Display) {
    mp.suspend(|| println!("{msg}"));
}

fn count_files(n: u64) -> String {
    if n == 1 {
        "1 file".to_string()
    } else {
        format!("{n} files")
    }
}

fn qr(data: &str) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(data.as_bytes()).ok()?;
    Some(
        code.render::<Dense1x2>()
            .dark_color(Dense1x2::Light)
            .light_color(Dense1x2::Dark)
            .quiet_zone(true)
            .build(),
    )
}

/// Bytes for one in-flight request: `done` is the estimate for blobs already
/// streamed in this request, `offset` the position inside the current one.
/// `UploadDone` replaces the estimate with the provider's real count.
#[derive(Default)]
struct RequestState {
    done: u64,
    offset: u64,
}

impl RequestState {
    fn estimate(&self) -> u64 {
        self.done + self.offset
    }
}

struct PeerState {
    bar: ProgressBar,
    label: String,
    done: u64,
    requests: HashMap<u64, RequestState>,
    sent_named: bool,
}

impl PeerState {
    fn position(&self, total: u64) -> u64 {
        let inflight: u64 = self.requests.values().map(RequestState::estimate).sum();
        (self.done + inflight).min(total)
    }
}

/// Render the sending side until the event channel closes.
pub async fn render_send(mut rx: EventReceiver, show_qr: bool, verbose: bool) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut hashing: Option<ProgressBar> = None;
    let mut hashing_offsets: HashMap<String, u64> = HashMap::new();
    let mut hashing_sizes: HashMap<String, u64> = HashMap::new();
    let mut hashed_bytes = 0u64;
    let mut total_files = 0u64;
    let mut total_bytes = 0u64;
    let mut peers: HashMap<u64, PeerState> = HashMap::new();

    while let Some(event) = rx.recv().await {
        match event {
            Event::ImportStarted { files, bytes } => {
                total_files = files;
                total_bytes = bytes;
                hashing = Some(bytes_bar(&mp, bytes, "Hashing".into()));
            }
            Event::ImportFileStarted { name, size } => {
                hashing_sizes.insert(name.clone(), size);
                if let Some(pb) = &hashing {
                    pb.set_message(style(name).dim().to_string());
                }
            }
            Event::ImportFileProgress { name, offset } => {
                hashing_offsets.insert(name, offset);
                if let Some(pb) = &hashing {
                    let inflight: u64 = hashing_offsets.values().sum();
                    pb.set_position((hashed_bytes + inflight).min(total_bytes));
                }
            }
            Event::ImportFileDone { name } => {
                hashing_offsets.remove(&name);
                hashed_bytes += hashing_sizes.remove(&name).unwrap_or(0);
                if let Some(pb) = &hashing {
                    let inflight: u64 = hashing_offsets.values().sum();
                    pb.set_position((hashed_bytes + inflight).min(total_bytes));
                }
            }
            Event::ImportDone { .. } => {
                if let Some(pb) = hashing.take() {
                    pb.finish_and_clear();
                }
            }
            Event::Ready {
                ticket,
                link,
                addrs,
            } => {
                let target = link.clone().unwrap_or_else(|| ticket.clone());
                let mut out = String::new();
                out.push_str(&format!(
                    "Sharing {} ({})\n\n",
                    style(count_files(total_files)).bold(),
                    HumanBytes(total_bytes)
                ));
                if let Some(link) = &link {
                    out.push_str(&format!("  {}\n\n", style(link).green().bold()));
                    out.push_str(&format!(
                        "  {} {}\n\n",
                        style("or run:").dim(),
                        style(format!("hither {ticket}")).dim()
                    ));
                } else {
                    out.push_str(&format!(
                        "  {}\n\n",
                        style(format!("hither {ticket}")).green().bold()
                    ));
                }
                if show_qr {
                    if let Some(code) = qr(&target) {
                        out.push_str(&code);
                        out.push('\n');
                    }
                }
                if verbose {
                    out.push_str(&format!(
                        "{}\n\n",
                        style(format!("Reachable via: {}", addrs.join(", "))).dim()
                    ));
                }
                out.push_str(&format!(
                    "{}",
                    style("Hie thee hither: keep this open until the other side has everything. Ctrl-C to stop.")
                        .dim()
                ));
                say_out(&mp, out);
            }
            Event::PeerConnected { connection, peer } => {
                let label = peer.unwrap_or_else(|| "peer".into());
                let bar = bytes_bar(&mp, total_bytes, label.clone());
                bar.set_message(style("connected").dim().to_string());
                peers.insert(
                    connection,
                    PeerState {
                        bar,
                        label,
                        done: 0,
                        requests: HashMap::new(),
                        sent_named: false,
                    },
                );
            }
            Event::PeerDisconnected { connection } => {
                if let Some(peer) = peers.remove(&connection) {
                    let pos = peer.position(total_bytes);
                    peer.bar.finish_and_clear();
                    mp.remove(&peer.bar);
                    if peer.sent_named && pos >= total_bytes {
                        say(
                            &mp,
                            format!(
                                "{} {} received everything ({})",
                                style("✓").green().bold(),
                                style(&peer.label).bold(),
                                HumanBytes(total_bytes)
                            ),
                        );
                    } else if peer.sent_named && pos > 0 {
                        say(
                            &mp,
                            format!(
                                "{} {} disconnected after receiving {} this session. If they were not done, running the same command resumes.",
                                style("!").yellow().bold(),
                                style(&peer.label).bold(),
                                HumanBytes(pos)
                            ),
                        );
                    }
                }
            }
            Event::UploadStarted {
                connection,
                request,
                name,
                size,
            } => {
                let _ = size;
                if let Some(peer) = peers.get_mut(&connection) {
                    let req = peer.requests.entry(request).or_default();
                    req.done += req.offset;
                    req.offset = 0;
                    if let Some(name) = name {
                        peer.sent_named = true;
                        peer.bar.set_message(style(name).dim().to_string());
                    }
                    peer.bar.set_position(peer.position(total_bytes));
                }
            }
            Event::UploadProgress {
                connection,
                request,
                offset,
            } => {
                if let Some(peer) = peers.get_mut(&connection) {
                    if let Some(req) = peer.requests.get_mut(&request) {
                        req.offset = offset;
                    }
                    peer.bar.set_position(peer.position(total_bytes));
                }
            }
            Event::UploadDone {
                connection,
                request,
                bytes,
            } => {
                if let Some(peer) = peers.get_mut(&connection) {
                    peer.requests.remove(&request);
                    peer.done += bytes;
                    peer.bar.set_position(peer.position(total_bytes));
                    if peer.position(total_bytes) >= total_bytes {
                        peer.bar.set_message(style("complete").green().to_string());
                    }
                }
            }
            Event::UploadAborted {
                connection,
                request,
            } => {
                if let Some(peer) = peers.get_mut(&connection) {
                    if let Some(req) = peer.requests.remove(&request) {
                        peer.done += req.estimate();
                    }
                    peer.bar.set_position(peer.position(total_bytes));
                }
            }
            _ => {}
        }
    }
    for (_, peer) in peers.drain() {
        peer.bar.finish_and_clear();
    }
}

/// Render the receiving side until the event channel closes.
pub async fn render_get(mut rx: EventReceiver) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut status: Option<ProgressBar> = None;
    let mut download: Option<ProgressBar> = None;
    let mut export: Option<ProgressBar> = None;
    let mut path_kind = PathKind::Unknown;
    let mut peer = String::new();
    let mut total_files = 0u64;

    while let Some(event) = rx.recv().await {
        match event {
            Event::Connecting => {
                status = Some(spinner(&mp, "Connecting to the sender…"));
            }
            Event::Connected { peer: id } => {
                peer = id;
                if let Some(pb) = &status {
                    pb.set_message(format!(
                        "Connected to {} · fetching the file list…",
                        style(&peer).bold()
                    ));
                }
            }
            Event::PathChanged { kind } => {
                path_kind = kind;
                if let Some(pb) = &download {
                    pb.set_prefix(path_label(kind));
                }
            }
            Event::ManifestReceived { files, bytes, have } => {
                total_files = files.len() as u64;
                if let Some(pb) = status.take() {
                    pb.finish_and_clear();
                }
                let resumed = if have > 0 {
                    format!(" · resuming, {} already here", HumanBytes(have))
                } else {
                    String::new()
                };
                say(
                    &mp,
                    format!(
                        "Receiving {} ({}) from {}{}",
                        style(count_files(total_files)).bold(),
                        HumanBytes(bytes),
                        style(&peer).bold(),
                        style(resumed).dim()
                    ),
                );
                let pb = bytes_bar(&mp, bytes, path_label(path_kind));
                pb.set_position(have.min(bytes));
                download = Some(pb);
            }
            Event::DownloadProgress { bytes, total } => {
                if let Some(pb) = &download {
                    pb.set_length(total);
                    pb.set_position(bytes);
                }
            }
            Event::DownloadDone { bytes, seconds } => {
                if let Some(pb) = download.take() {
                    pb.finish_and_clear();
                }
                if bytes > 0 && seconds > 0.0 {
                    say(
                        &mp,
                        format!(
                            "{} Downloaded and verified {} in {} ({}/s, {})",
                            style("✓").green().bold(),
                            HumanBytes(bytes),
                            HumanDuration(Duration::from_secs_f64(seconds)),
                            HumanBytes((bytes as f64 / seconds) as u64),
                            path_label(path_kind).to_lowercase()
                        ),
                    );
                }
                let pb = mp.add(ProgressBar::new(total_files));
                pb.set_style(
                    ProgressStyle::with_template(
                        "{prefix:>10} [{bar:30.cyan/dim}] {pos}/{len} {msg}",
                    )
                    .unwrap()
                    .progress_chars("━╸─"),
                );
                pb.set_prefix("Writing");
                export = Some(pb);
            }
            Event::ExportFileStarted { name, .. } => {
                if let Some(pb) = &export {
                    pb.set_message(style(name).dim().to_string());
                }
            }
            Event::ExportFileDone { .. } => {
                if let Some(pb) = &export {
                    pb.inc(1);
                }
            }
            Event::Finished { files, bytes, dir } => {
                if let Some(pb) = export.take() {
                    pb.finish_and_clear();
                }
                say(
                    &mp,
                    format!(
                        "{} Saved {} ({}) to {}",
                        style("✓").green().bold(),
                        style(count_files(files)).bold(),
                        HumanBytes(bytes),
                        style(dir.display()).bold()
                    ),
                );
            }
            _ => {}
        }
    }
    for pb in [status, download, export].into_iter().flatten() {
        pb.finish_and_clear();
    }
}

fn path_label(kind: PathKind) -> String {
    match kind {
        PathKind::Direct => "Direct".into(),
        PathKind::Relay => "Relayed".into(),
        PathKind::Unknown => "Receiving".into(),
    }
}
