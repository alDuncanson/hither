//! Terminal rendering of the core's event stream.
//!
//! Three views are shared between commands: hashing (sender side), peers
//! uploading (sender side), and a receive in progress. Each command's
//! renderer owns a `MultiProgress` and dispatches events to the views it
//! needs, printing its own headline lines in between.

use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};

use console::style;
use hither_core::{Event, EventReceiver, PathKind, inbox::Decider};
use indicatif::{
    HumanBytes, HumanDuration, MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle,
};
use tokio::io::AsyncBufReadExt;

const TICK: Duration = Duration::from_millis(120);

// --------------------------------------------------------------- helpers

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

/// The one thing scripts want to capture, on stdout.
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

fn path_label(kind: PathKind) -> String {
    match kind {
        PathKind::Direct => "Direct".into(),
        PathKind::Relay => "Relayed".into(),
        PathKind::Unknown => "Receiving".into(),
    }
}

// ------------------------------------------------------- view: hashing

#[derive(Default)]
struct HashingView {
    bar: Option<ProgressBar>,
    offsets: HashMap<String, u64>,
    sizes: HashMap<String, u64>,
    done: u64,
    pub total_files: u64,
    pub total_bytes: u64,
}

impl HashingView {
    /// Returns true if the event was consumed.
    fn handle(&mut self, mp: &MultiProgress, ev: &Event) -> bool {
        match ev {
            Event::ImportStarted { files, bytes } => {
                self.total_files = *files;
                self.total_bytes = *bytes;
                self.bar = Some(bytes_bar(mp, *bytes, "Hashing".into()));
            }
            Event::ImportFileStarted { name, size } => {
                self.sizes.insert(name.clone(), *size);
                if let Some(pb) = &self.bar {
                    pb.set_message(style(name).dim().to_string());
                }
            }
            Event::ImportFileProgress { name, offset } => {
                self.offsets.insert(name.clone(), *offset);
                self.redraw();
            }
            Event::ImportFileDone { name } => {
                self.offsets.remove(name);
                self.done += self.sizes.remove(name).unwrap_or(0);
                self.redraw();
            }
            Event::ImportDone { .. } => {
                if let Some(pb) = self.bar.take() {
                    pb.finish_and_clear();
                }
            }
            _ => return false,
        }
        true
    }

    fn redraw(&self) {
        if let Some(pb) = &self.bar {
            let inflight: u64 = self.offsets.values().sum();
            pb.set_position((self.done + inflight).min(self.total_bytes));
        }
    }
}

// --------------------------------------------------------- view: peers

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

#[derive(Default)]
struct PeersView {
    peers: HashMap<u64, PeerState>,
    /// Print a line when a peer disconnects (the plain share wants it; an
    /// inbox delivery reports completion through its own message instead).
    announce_disconnects: bool,
}

impl PeersView {
    fn handle(&mut self, mp: &MultiProgress, ev: &Event, total: u64) -> bool {
        match ev {
            Event::PeerConnected { connection, peer } => {
                let label = peer.clone().unwrap_or_else(|| "peer".into());
                let bar = bytes_bar(mp, total, label.clone());
                bar.set_message(style("connected").dim().to_string());
                self.peers.insert(
                    *connection,
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
                if let Some(peer) = self.peers.remove(connection) {
                    let pos = peer.position(total);
                    peer.bar.finish_and_clear();
                    mp.remove(&peer.bar);
                    if self.announce_disconnects && peer.sent_named {
                        if pos >= total {
                            say(
                                mp,
                                format!(
                                    "{} {} received everything ({})",
                                    style("✓").green().bold(),
                                    style(&peer.label).bold(),
                                    HumanBytes(total)
                                ),
                            );
                        } else if pos > 0 {
                            say(
                                mp,
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
            }
            Event::UploadStarted {
                connection,
                request,
                name,
                ..
            } => {
                if let Some(peer) = self.peers.get_mut(connection) {
                    let req = peer.requests.entry(*request).or_default();
                    req.done += req.offset;
                    req.offset = 0;
                    if let Some(name) = name {
                        peer.sent_named = true;
                        peer.bar.set_message(style(name).dim().to_string());
                    }
                    peer.bar.set_position(peer.position(total));
                }
            }
            Event::UploadProgress {
                connection,
                request,
                offset,
            } => {
                if let Some(peer) = self.peers.get_mut(connection) {
                    if let Some(req) = peer.requests.get_mut(request) {
                        req.offset = *offset;
                    }
                    peer.bar.set_position(peer.position(total));
                }
            }
            Event::UploadDone {
                connection,
                request,
                bytes,
            } => {
                if let Some(peer) = self.peers.get_mut(connection) {
                    peer.requests.remove(request);
                    peer.done += bytes;
                    peer.bar.set_position(peer.position(total));
                    if peer.position(total) >= total {
                        peer.bar.set_message(style("complete").green().to_string());
                    }
                }
            }
            Event::UploadAborted {
                connection,
                request,
            } => {
                if let Some(peer) = self.peers.get_mut(connection) {
                    if let Some(req) = peer.requests.remove(request) {
                        peer.done += req.estimate();
                    }
                    peer.bar.set_position(peer.position(total));
                }
            }
            _ => return false,
        }
        true
    }

    fn clear(&mut self) {
        for (_, peer) in self.peers.drain() {
            peer.bar.finish_and_clear();
        }
    }
}

// ------------------------------------------------------- view: receive

#[derive(Default)]
struct ReceiveView {
    status: Option<ProgressBar>,
    download: Option<ProgressBar>,
    export: Option<ProgressBar>,
    path_kind: Option<PathKind>,
    peer: String,
    total_files: u64,
    /// Where to say the files went; the inbox names the folder itself.
    quiet_finish: bool,
}

impl ReceiveView {
    fn kind(&self) -> PathKind {
        self.path_kind.unwrap_or(PathKind::Unknown)
    }

    fn handle(&mut self, mp: &MultiProgress, ev: &Event) -> bool {
        match ev {
            Event::Connecting => {
                self.status = Some(spinner(mp, "Connecting to the sender…"));
            }
            Event::Connected { peer } => {
                self.peer = peer.clone();
                if let Some(pb) = &self.status {
                    pb.set_message(format!(
                        "Connected to {} · fetching the file list…",
                        style(&self.peer).bold()
                    ));
                }
            }
            Event::PathChanged { kind } => {
                self.path_kind = Some(*kind);
                if let Some(pb) = &self.download {
                    pb.set_prefix(path_label(*kind));
                }
            }
            Event::ManifestReceived { files, bytes, have } => {
                self.total_files = files.len() as u64;
                if let Some(pb) = self.status.take() {
                    pb.finish_and_clear();
                }
                let resumed = if *have > 0 {
                    format!(" · resuming, {} already here", HumanBytes(*have))
                } else {
                    String::new()
                };
                say(
                    mp,
                    format!(
                        "Receiving {} ({}) from {}{}",
                        style(count_files(self.total_files)).bold(),
                        HumanBytes(*bytes),
                        style(&self.peer).bold(),
                        style(resumed).dim()
                    ),
                );
                let pb = bytes_bar(mp, *bytes, path_label(self.kind()));
                pb.set_position((*have).min(*bytes));
                self.download = Some(pb);
            }
            Event::DownloadProgress { bytes, total } => {
                if let Some(pb) = &self.download {
                    pb.set_length(*total);
                    pb.set_position(*bytes);
                }
            }
            Event::DownloadDone { bytes, seconds } => {
                if let Some(pb) = self.download.take() {
                    pb.finish_and_clear();
                }
                if *bytes > 0 && *seconds > 0.0 {
                    let how = match self.kind() {
                        PathKind::Relay => "via relay; `hither doctor` explains why".to_string(),
                        other => path_label(other).to_lowercase(),
                    };
                    say(
                        mp,
                        format!(
                            "{} Downloaded and verified {} in {} ({}/s, {})",
                            style("✓").green().bold(),
                            HumanBytes(*bytes),
                            HumanDuration(Duration::from_secs_f64(*seconds)),
                            HumanBytes((*bytes as f64 / seconds) as u64),
                            how
                        ),
                    );
                }
                let pb = mp.add(ProgressBar::new(self.total_files));
                pb.set_style(
                    ProgressStyle::with_template(
                        "{prefix:>10} [{bar:30.cyan/dim}] {pos}/{len} {msg}",
                    )
                    .unwrap()
                    .progress_chars("━╸─"),
                );
                pb.set_prefix("Writing");
                self.export = Some(pb);
            }
            Event::ExportFileStarted { name, .. } => {
                if let Some(pb) = &self.export {
                    pb.set_message(style(name).dim().to_string());
                }
            }
            Event::ExportFileDone { .. } => {
                if let Some(pb) = &self.export {
                    pb.inc(1);
                }
            }
            Event::Finished { files, bytes, dir } => {
                if let Some(pb) = self.export.take() {
                    pb.finish_and_clear();
                }
                if !self.quiet_finish {
                    say(
                        mp,
                        format!(
                            "{} Saved {} ({}) to {}",
                            style("✓").green().bold(),
                            style(count_files(*files)).bold(),
                            HumanBytes(*bytes),
                            style(dir.display()).bold()
                        ),
                    );
                }
            }
            _ => return false,
        }
        true
    }

    fn clear(&mut self) {
        for pb in [self.status.take(), self.download.take(), self.export.take()]
            .into_iter()
            .flatten()
        {
            pb.finish_and_clear();
        }
    }
}

// ------------------------------------------------------------ renderers

/// `hither <paths>`: hash, print the ticket, show peers pulling.
pub async fn render_send(mut rx: EventReceiver, show_qr: bool, verbose: bool) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut hashing = HashingView::default();
    let mut peers = PeersView {
        announce_disconnects: true,
        ..Default::default()
    };
    while let Some(ev) = rx.recv().await {
        if hashing.handle(&mp, &ev) || peers.handle(&mp, &ev, hashing.total_bytes) {
            continue;
        }
        if let Event::Ready {
            ticket,
            link,
            addrs,
        } = &ev
        {
            let target = link.clone().unwrap_or_else(|| ticket.clone());
            let mut out = String::new();
            out.push_str(&format!(
                "Sharing {} ({})\n\n",
                style(count_files(hashing.total_files)).bold(),
                HumanBytes(hashing.total_bytes)
            ));
            if let Some(link) = link {
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
    }
    peers.clear();
}

/// `hither <ticket>`: receive into a folder.
pub async fn render_get(mut rx: EventReceiver) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut view = ReceiveView::default();
    while let Some(ev) = rx.recv().await {
        view.handle(&mp, &ev);
    }
    view.clear();
}

/// `hither to <inbox> <paths>`: hash, announce, wait for acceptance, upload.
pub async fn render_to(mut rx: EventReceiver, verbose: bool) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut hashing = HashingView::default();
    let mut peers = PeersView::default();
    let mut waiting: Option<ProgressBar> = None;
    while let Some(ev) = rx.recv().await {
        if hashing.handle(&mp, &ev) || peers.handle(&mp, &ev, hashing.total_bytes) {
            continue;
        }
        match &ev {
            Event::Ready { addrs, .. } => {
                if verbose {
                    say(
                        &mp,
                        style(format!("Reachable via: {}", addrs.join(", "))).dim(),
                    );
                }
            }
            Event::OfferSent { to } => {
                waiting = Some(spinner(
                    &mp,
                    &format!(
                        "Offered {} ({}) to {}. Waiting for them to accept…",
                        count_files(hashing.total_files),
                        HumanBytes(hashing.total_bytes),
                        style(to).bold()
                    ),
                ));
            }
            Event::ToAccepted => {
                if let Some(pb) = waiting.take() {
                    pb.finish_and_clear();
                }
                say(
                    &mp,
                    format!("{} They accepted. Sending…", style("✓").green().bold()),
                );
            }
            Event::ToDeclined { reason } => {
                if let Some(pb) = waiting.take() {
                    pb.finish_and_clear();
                }
                let why = if reason.is_empty() {
                    String::new()
                } else {
                    format!(": {reason}")
                };
                say(
                    &mp,
                    format!("{} They declined{why}.", style("✗").red().bold()),
                );
            }
            Event::ToDone { files, bytes } => {
                peers.clear();
                say(
                    &mp,
                    format!(
                        "{} Delivered {} ({}). They have everything.",
                        style("✓").green().bold(),
                        style(count_files(*files)).bold(),
                        HumanBytes(*bytes)
                    ),
                );
            }
            _ => {}
        }
    }
    peers.clear();
}

/// `hither inbox`: print the link, show offers, prompt, show each pull.
pub async fn render_inbox(
    mut rx: EventReceiver,
    decider: Decider,
    show_qr: bool,
    verbose: bool,
    policy_note: &'static str,
) {
    let mp = MultiProgress::with_draw_target(ProgressDrawTarget::stderr());
    let mut receive = ReceiveView {
        quiet_finish: true,
        ..Default::default()
    };
    let mut awaiting_answer: VecDeque<u64> = VecDeque::new();
    let mut stdin = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    let _ = verbose;

    loop {
        tokio::select! {
            ev = rx.recv() => {
                let Some(ev) = ev else { break };
                if receive.handle(&mp, &ev) {
                    continue;
                }
                match &ev {
                    Event::InboxReady { ticket, link, endpoint_id } => {
                        let target = link.clone().unwrap_or_else(|| ticket.clone());
                        let mut out = String::new();
                        out.push_str(&format!("{}\n\n", style("Your inbox is open.").bold()));
                        if let Some(link) = link {
                            out.push_str(&format!("  {}\n\n", style(link).green().bold()));
                            out.push_str(&format!("  {} {}\n\n", style("or:").dim(), style(format!("hither to {ticket} <files>")).dim()));
                        } else {
                            out.push_str(&format!("  {}\n\n", style(format!("hither to {ticket} <files>")).green().bold()));
                        }
                        if show_qr {
                            if let Some(code) = qr(&target) {
                                out.push_str(&code);
                                out.push('\n');
                            }
                        }
                        out.push_str(&format!(
                            "{}\n{}\n{}\n{}",
                            style(format!("Anyone holding this link can offer you files. {policy_note}")).dim(),
                            style(format!("On another machine you own, save it once: hither friends add <name> {ticket}")).dim(),
                            style(format!("Your endpoint id is {endpoint_id}.")).dim(),
                            style("Ctrl-C closes the inbox.").dim()
                        ));
                        say_out(&mp, out);
                    }
                    Event::Offer {
                        id,
                        from_short,
                        label,
                        files,
                        bytes,
                        pending,
                        ..
                    } => {
                        let who = match label {
                            Some(l) => format!("{} ({from_short})", style(l).bold()),
                            None => style(from_short).bold().to_string(),
                        };
                        let mut out = format!(
                            "\n{} Tidings from {who}: {} ({})\n",
                            style("✉").cyan().bold(),
                            style(count_files(files.len() as u64)).bold(),
                            HumanBytes(*bytes)
                        );
                        for f in files.iter().take(6) {
                            out.push_str(&format!("    {}  {}\n", style(HumanBytes(f.size)).dim(), f.name));
                        }
                        if files.len() > 6 {
                            out.push_str(&format!("    {}\n", style(format!("… and {} more", files.len() - 6)).dim()));
                        }
                        if *pending {
                            out.push_str(&format!("  {} ", style("Accept? [y/N]").bold()));
                            awaiting_answer.push_back(*id);
                            mp.suspend(|| eprint!("{out}"));
                        } else {
                            say(&mp, out.trim_end());
                        }
                    }
                    Event::OfferAccepted { .. } => {
                        say(&mp, format!("{} Accepted. Bringing them hither…", style("✓").green().bold()));
                    }
                    Event::OfferDeclined { .. } => {
                        say(&mp, format!("{} Declined.", style("✗").dim()));
                    }
                    Event::OfferStarted { .. } => {}
                    Event::OfferDone { files, bytes, dir, .. } => {
                        say(&mp, format!(
                            "{} Saved {} ({}) to {}\n",
                            style("✓").green().bold(),
                            style(count_files(*files)).bold(),
                            HumanBytes(*bytes),
                            style(dir.display()).bold()
                        ));
                    }
                    Event::OfferFailed { reason, .. } => {
                        receive.clear();
                        say(&mp, format!("{} Could not finish: {reason}\n", style("✗").red().bold()));
                    }
                    _ => {}
                }
            }
            line = stdin.next_line(), if !awaiting_answer.is_empty() => {
                let Ok(Some(line)) = line else { break };
                if let Some(id) = awaiting_answer.pop_front() {
                    let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "aye");
                    decider.decide(id, yes);
                }
            }
        }
    }
    receive.clear();
}

/// Render the doctor report as a checklist.
pub fn render_doctor(r: &hither_core::DoctorReport) {
    use hither_core::doctor::Status;
    let mark = |s: Status| match s {
        Status::Ok => style("✓").green().bold().to_string(),
        Status::Failed => style("✗").red().bold().to_string(),
        Status::Skipped => style("–").dim().to_string(),
        Status::Unknown => style("?").yellow().bold().to_string(),
    };
    let line = |m: String, label: &str, detail: String| {
        println!("{m} {:<22} {}", label, style(detail).dim());
    };

    match &r.identity {
        Some(id) => line(
            mark(Status::Ok),
            "identity",
            format!(
                "{}{}",
                id.endpoint_id,
                id.path
                    .as_ref()
                    .map(|p| format!("  ({p})"))
                    .unwrap_or_default()
            ),
        ),
        None => line(
            mark(Status::Skipped),
            "identity",
            "none yet; `hither id` creates one".into(),
        ),
    }
    line(
        mark(r.udp.loopback),
        "udp loopback",
        "127.0.0.1 round trip".into(),
    );
    line(
        mark(r.udp.lan),
        "udp to own address",
        r.udp
            .lan_ip
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "no outbound route".into()),
    );
    line(
        mark(r.relay.status),
        "relay",
        match (&r.relay.url, r.relay.millis) {
            (Some(u), Some(ms)) => format!("{u}  {ms} ms"),
            (Some(u), None) => u.clone(),
            _ => "unreachable".into(),
        },
    );
    if let Some(nat) = &r.nat {
        let nat_kind = match nat.symmetric_nat {
            Some(true) => "symmetric (hard)",
            Some(false) => "consistent mapping (good)",
            None => "unknown",
        };
        line(
            mark(match nat.symmetric_nat {
                Some(true) => Status::Failed,
                Some(false) => Status::Ok,
                None => Status::Unknown,
            }),
            "nat",
            nat_kind.into(),
        );
        line(
            mark(match nat.public_v4.as_ref().or(nat.public_v6.as_ref()) {
                Some(_) => Status::Ok,
                None => Status::Unknown,
            }),
            "public address",
            [nat.public_v4.clone(), nat.public_v6.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", "),
        );
        if let Some((url, ms)) = nat.relay_latencies.first() {
            line(mark(Status::Ok), "nearest relay", format!("{url}  {ms} ms"));
        }
    }
    if !r.addrs.is_empty() {
        line(mark(Status::Ok), "ticket would carry", r.addrs.join(", "));
    }
    println!();
    let verdict = match r.verdict {
        hither_core::Verdict::DirectLikely => {
            style("Direct connections should work.").green().bold()
        }
        hither_core::Verdict::RelayOnly => style("Transfers will go via relay on this network.")
            .yellow()
            .bold(),
        hither_core::Verdict::Offline => style("Nothing can connect from here right now.")
            .red()
            .bold(),
    };
    println!("{verdict}");
    for reason in &r.reasons {
        println!("  {} {reason}", style("·").dim());
    }
    println!(
        "{}",
        style(format!(
            "checked in {:.1}s",
            r.elapsed_millis as f64 / 1000.0
        ))
        .dim()
    );
}
