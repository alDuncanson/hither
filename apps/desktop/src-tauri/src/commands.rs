//! Everything the window can ask for. Each command maps onto one core call;
//! progress comes back through `hither-event` events carrying an
//! [`Envelope`], so the front end can tell which share or inbox an event
//! belongs to.

use std::{collections::HashMap, path::PathBuf};

use hither_core::{
    AcceptPolicy, CancellationToken, Code, DoctorOptions, DoctorReport, Event, EventSender,
    Friends, Identity, Inbox, InboxOptions, NetOptions, ReceiveOptions, SendOptions, Sender,
    TicketKind, friends, inbox,
    link::{self, Link},
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::Mutex;

/// Where printed links point; same as the CLI.
const LINK_BASE: &str = "https://alduncanson.github.io/hither/";

#[derive(Default)]
pub struct AppState {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    shares: HashMap<u64, Sender>,
    next_share: u64,
    inbox: Option<Inbox>,
    decider: Option<inbox::Decider>,
}

/// A core event plus where it came from.
#[derive(Serialize, Clone)]
struct Envelope<'a> {
    source: &'static str,
    id: u64,
    event: &'a Event,
}

type CmdResult<T> = Result<T, String>;

fn err(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Forward every event from one operation to the window, plus a desktop
/// notification for the two moments a person may not be looking.
fn forward(app: AppHandle, source: &'static str, id: u64) -> EventSender {
    let (tx, mut rx) = hither_core::channel();
    tauri::async_runtime::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match &ev {
                Event::Offer {
                    from_short,
                    label,
                    files,
                    bytes,
                    ..
                } => {
                    let who = label.clone().unwrap_or_else(|| from_short.clone());
                    let _ = app
                        .notification()
                        .builder()
                        .title(format!("Tidings from {who}"))
                        .body(format!(
                            "{} files, {}",
                            files.len(),
                            indicatif_bytes(*bytes)
                        ))
                        .show();
                }
                Event::OfferDone { files, bytes, .. } => {
                    let _ = app
                        .notification()
                        .builder()
                        .title("Received")
                        .body(format!("{files} files, {}", indicatif_bytes(*bytes)))
                        .show();
                }
                _ => {}
            }
            let _ = app.emit(
                "hither-event",
                Envelope {
                    source,
                    id,
                    event: &ev,
                },
            );
        }
    });
    tx
}

fn indicatif_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

// ------------------------------------------------------------------ share

#[derive(Serialize)]
pub struct ShareInfo {
    pub id: u64,
    pub ticket: String,
    pub link: Option<String>,
    pub code: Option<String>,
    pub files: u64,
    pub bytes: u64,
}

#[tauri::command]
pub async fn share(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    code: bool,
) -> CmdResult<ShareInfo> {
    let id = {
        let mut inner = state.inner.lock().await;
        inner.next_share += 1;
        inner.next_share
    };
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let sender = Sender::start(
        &paths,
        SendOptions {
            net: NetOptions::default(),
            ticket_kind: TicketKind::Full,
            link_base: Some(LINK_BASE.into()),
            code,
            ..SendOptions::default()
        },
        forward(app, "share", id),
    )
    .await
    .map_err(err)?;
    let info = ShareInfo {
        id,
        ticket: sender.ticket().to_string(),
        link: sender.link().map(str::to_string),
        code: sender.code().map(|c| c.to_string()),
        files: sender.files().len() as u64,
        bytes: sender.total_bytes(),
    };
    state.inner.lock().await.shares.insert(id, sender);
    Ok(info)
}

#[tauri::command]
pub async fn stop_share(state: State<'_, AppState>, id: u64) -> CmdResult<()> {
    let sender = state.inner.lock().await.shares.remove(&id);
    if let Some(sender) = sender {
        sender.shutdown().await.map_err(err)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- receive

#[derive(Serialize)]
pub struct ReceiveInfo {
    pub dir: String,
    pub files: u64,
    pub bytes: u64,
}

/// Receive from a link, ticket, or four-word code. Runs to completion;
/// progress arrives as events with `source: "receive"`.
#[tauri::command]
pub async fn receive(
    app: AppHandle,
    input: String,
    out_dir: Option<String>,
) -> CmdResult<ReceiveInfo> {
    let net = NetOptions::default();
    let ticket = if let Ok(code) = Code::parse(&input) {
        let payload = hither_core::code::redeem(&code, &net).await.map_err(err)?;
        match link::parse_any(&payload).map_err(err)? {
            Link::Share(t) => t,
            Link::Inbox(_) => return Err("that code belongs to an inbox".into()),
        }
    } else {
        link::parse(&input).map_err(err)?
    };
    let out_dir = out_dir.map(PathBuf::from).unwrap_or_else(default_dir_path);
    let received = hither_core::receive(
        ticket,
        ReceiveOptions { out_dir, net },
        forward(app, "receive", 0),
        CancellationToken::new(),
    )
    .await
    .map_err(err)?;
    Ok(ReceiveInfo {
        dir: received.dir.display().to_string(),
        files: received.files.len() as u64,
        bytes: received.bytes,
    })
}

// ------------------------------------------------------------------ inbox

#[derive(Serialize)]
pub struct InboxInfo {
    pub ticket: String,
    pub link: Option<String>,
    pub endpoint_id: String,
    pub dir: String,
}

#[tauri::command]
pub async fn inbox_open(
    app: AppHandle,
    state: State<'_, AppState>,
    dir: Option<String>,
    accept_all: bool,
) -> CmdResult<InboxInfo> {
    let mut inner = state.inner.lock().await;
    if inner.inbox.is_some() {
        return Err("the inbox is already open".into());
    }
    let identity = Identity::load_default().map_err(err)?;
    let token = inbox::load_or_create_token(false).map_err(err)?;
    let dir = dir.map(PathBuf::from).unwrap_or_else(default_dir_path);
    let inbox = Inbox::open(
        &identity,
        token,
        InboxOptions {
            dir: dir.clone(),
            net: NetOptions::default(),
            policy: if accept_all {
                AcceptPolicy::AcceptAll
            } else {
                AcceptPolicy::Ask
            },
            link_base: Some(LINK_BASE.into()),
        },
        forward(app, "inbox", 0),
    )
    .await
    .map_err(err)?;
    let info = InboxInfo {
        ticket: inbox.ticket().to_string(),
        link: inbox.link().map(str::to_string),
        endpoint_id: identity.endpoint_id().to_string(),
        dir: dir.display().to_string(),
    };
    inner.decider = Some(inbox.decider());
    inner.inbox = Some(inbox);
    Ok(info)
}

#[tauri::command]
pub async fn inbox_close(state: State<'_, AppState>) -> CmdResult<()> {
    let inbox = {
        let mut inner = state.inner.lock().await;
        inner.decider = None;
        inner.inbox.take()
    };
    if let Some(inbox) = inbox {
        inbox.shutdown().await.map_err(err)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn inbox_decide(state: State<'_, AppState>, id: u64, accept: bool) -> CmdResult<bool> {
    let inner = state.inner.lock().await;
    match &inner.decider {
        Some(d) => Ok(d.decide(id, accept)),
        None => Err("the inbox is not open".into()),
    }
}

// ------------------------------------------------------------- send to

#[derive(Serialize)]
pub struct Delivered {
    pub files: u64,
    pub bytes: u64,
}

/// Offer files to a saved friend or an inbox link.
#[tauri::command]
pub async fn send_to(
    app: AppHandle,
    target: String,
    paths: Vec<String>,
    label: Option<String>,
) -> CmdResult<Delivered> {
    let inbox = friends::resolve_inbox(&target).map_err(err)?;
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    let delivered = hither_core::send_to(
        &inbox,
        &paths,
        label,
        SendOptions::default(),
        forward(app, "to", 0),
        CancellationToken::new(),
    )
    .await
    .map_err(err)?;
    Ok(Delivered {
        files: delivered.files,
        bytes: delivered.bytes,
    })
}

// ---------------------------------------------------------------- friends

#[derive(Serialize)]
pub struct FriendInfo {
    pub name: String,
    pub short_id: String,
}

#[tauri::command]
pub fn friends() -> CmdResult<Vec<FriendInfo>> {
    let book = Friends::load().map_err(err)?;
    Ok(book
        .inboxes
        .keys()
        .map(|name| FriendInfo {
            name: name.clone(),
            short_id: book
                .get(name)
                .map(|t| t.endpoint_id().fmt_short().to_string())
                .unwrap_or_default(),
        })
        .collect())
}

#[tauri::command]
pub fn friend_add(name: String, link: String) -> CmdResult<()> {
    let ticket = link::parse_inbox(&link).map_err(err)?;
    let mut book = Friends::load().map_err(err)?;
    book.add(&name, &ticket).map_err(err)?;
    book.save().map_err(err)
}

#[tauri::command]
pub fn friend_remove(name: String) -> CmdResult<bool> {
    let mut book = Friends::load().map_err(err)?;
    let removed = book.remove(&name);
    book.save().map_err(err)?;
    Ok(removed)
}

// ------------------------------------------------------ identity, doctor

#[derive(Serialize)]
pub struct IdentityInfo {
    pub endpoint_id: String,
    pub short_id: String,
    pub path: Option<String>,
}

#[tauri::command]
pub fn identity() -> CmdResult<IdentityInfo> {
    let id = Identity::load_default().map_err(err)?;
    Ok(IdentityInfo {
        endpoint_id: id.endpoint_id().to_string(),
        short_id: id.endpoint_id().fmt_short().to_string(),
        path: id.path().map(|p| p.display().to_string()),
    })
}

/// The private key as 24 words (or hex). The front end shows it once, on
/// request, with a warning.
#[tauri::command]
pub fn identity_export(hex: bool) -> CmdResult<String> {
    let id = Identity::load_default().map_err(err)?;
    Ok(if hex { id.to_hex() } else { id.to_words() })
}

#[tauri::command]
pub async fn doctor() -> CmdResult<DoctorReport> {
    hither_core::diagnose(DoctorOptions::default())
        .await
        .map_err(err)
}

fn default_dir_path() -> PathBuf {
    dirs::download_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("hither")
}

#[tauri::command]
pub fn default_dir() -> String {
    default_dir_path().display().to_string()
}
