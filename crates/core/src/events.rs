//! Progress and status events emitted by the core.
//!
//! Every front end (CLI, desktop, mobile, web) consumes the same stream of
//! [`Event`]s. The enum is `serde`-serializable so it can cross an IPC or FFI
//! boundary unchanged (Tauri channels, uniffi callbacks, JSON over a socket).
//! Nothing in here knows about terminals or UI toolkits.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One file inside a share, addressed by its path relative to the share root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative path using `/` separators, e.g. `scans/roll-12/0007.tiff`.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
}

/// How the receiver is currently reaching the sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathKind {
    /// At least one direct (hole-punched or LAN) path is open.
    Direct,
    /// Only a relay path is open; bytes are being forwarded by a relay server.
    Relay,
    /// No path information yet.
    Unknown,
}

/// Progress events. Tagged so a JSON consumer can switch on `type`.
///
/// Which command produces what, so a UI author knows what to render:
///
/// | command | events |
/// |---|---|
/// | `hither <paths>` | `Import*`, `Ready`, then `Peer*` and `Upload*` per receiver |
/// | `hither <ticket>` | `Connecting`, `Connected`, `PathChanged`, `ManifestReceived`, `Download*`, `Export*`, `Finished` |
/// | `hither inbox` | `InboxReady`, then per offer `Offer`, `OfferAccepted`/`OfferDeclined`, `OfferStarted`, the receive events above, `OfferDone`/`OfferFailed` |
/// | `hither to` | `Import*`, `Ready`, `OfferSent`, `ToAccepted`/`ToDeclined`, `Peer*`/`Upload*`, `ToDone` |
///
/// The enum is deliberately flat: one list is easier to serialise and to
/// match on from JavaScript or Swift than nested enums would be.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    // ---------------------------------------------------------------- sender
    /// Hashing of the selected files is starting.
    ImportStarted {
        files: u64,
        bytes: u64,
    },
    /// A single file is being hashed.
    ImportFileStarted {
        name: String,
        size: u64,
    },
    /// Bytes hashed so far for one file.
    ImportFileProgress {
        name: String,
        offset: u64,
    },
    /// A single file has been hashed.
    ImportFileDone {
        name: String,
    },
    /// All files are hashed and the collection is stored.
    ImportDone {
        hash: String,
        files: u64,
        bytes: u64,
    },
    /// The sender is reachable. `ticket` is always present; `link` is the
    /// same ticket wrapped in a URL when a link base was configured.
    /// `addrs` lists how the ticket says we can be reached, for diagnostics.
    Ready {
        ticket: String,
        link: Option<String>,
        /// Four words that stand in for the ticket, when `--code` was used.
        code: Option<String>,
        addrs: Vec<String>,
    },
    /// A peer opened a connection.
    PeerConnected {
        connection: u64,
        peer: Option<String>,
    },
    /// A peer's connection closed. `bytes_sent` is the payload it actually

    /// received over this connection; compare with the share's total.
    PeerDisconnected {
        connection: u64,
        bytes_sent: u64,
    },
    /// A peer began pulling one blob. `name` is `None` for the collection's
    /// own metadata blobs.
    UploadStarted {
        connection: u64,
        request: u64,
        name: Option<String>,
        size: u64,
    },
    /// Bytes sent so far for the blob currently being uploaded on a request.
    UploadProgress {
        connection: u64,
        request: u64,
        offset: u64,
    },
    /// A request finished successfully. `bytes` is the payload actually
    /// sent for it, which can be far less than the blob sizes announced by
    /// `UploadStarted` when the receiver only probed for sizes.
    UploadDone {
        connection: u64,
        request: u64,
        bytes: u64,
    },
    /// A request was aborted by either side.
    UploadAborted {
        connection: u64,
        request: u64,
    },

    // -------------------------------------------------------------- receiver
    /// Dialing the sender.
    Connecting,
    /// Connected to the sender.
    Connected {
        peer: String,
    },
    /// The connection's path kind changed (relay vs direct).
    PathChanged {
        kind: PathKind,
    },
    /// The file list is known. `have` is how many bytes were already present
    /// locally from an earlier interrupted run.
    ManifestReceived {
        files: Vec<FileEntry>,
        bytes: u64,
        have: u64,
    },
    /// Cumulative bytes downloaded, including previously present bytes.
    DownloadProgress {
        bytes: u64,
        total: u64,
    },
    /// All bytes are verified and stored locally.
    DownloadDone {
        bytes: u64,
        seconds: f64,
    },
    /// Writing one file to its final destination.
    ExportFileStarted {
        name: String,
        size: u64,
    },
    /// Bytes written so far for one file.
    ExportFileProgress {
        name: String,
        offset: u64,
    },
    /// One file is in place.
    ExportFileDone {
        name: String,
    },
    /// Everything is on disk. `into` lists the top-level names that were
    /// written under `dir`, after any renaming to avoid existing files.
    Finished {
        files: u64,
        bytes: u64,
        dir: PathBuf,
        into: Vec<String>,
    },

    // ----------------------------------------------------------------- inbox
    /// The inbox is reachable and its link can be handed out.
    InboxReady {
        ticket: String,
        link: Option<String>,
        endpoint_id: String,
    },
    /// Someone offered files. Answer with `Inbox::decide` unless the policy
    /// already did.
    Offer {
        id: u64,
        from: String,
        from_short: String,
        label: Option<String>,
        files: Vec<FileEntry>,
        bytes: u64,
        /// True if the inbox is waiting for a decision; false if its policy
        /// already accepted.
        pending: bool,
    },
    OfferAccepted {
        id: u64,
    },
    OfferDeclined {
        id: u64,
        reason: String,
    },
    /// The pull for an accepted offer has started; the usual receive events
    /// follow until `OfferDone` or `OfferFailed`.
    OfferStarted {
        id: u64,
        dir: PathBuf,
    },
    OfferDone {
        id: u64,
        files: u64,
        bytes: u64,
        dir: PathBuf,
    },
    OfferFailed {
        id: u64,
        reason: String,
    },

    // -------------------------------------------------- sender to an inbox
    /// The announce was delivered; waiting for the other side to decide.
    OfferSent {
        to: String,
    },
    ToAccepted,
    ToDeclined {
        reason: String,
    },
    /// The inbox confirmed it has everything.
    ToDone {
        files: u64,
        bytes: u64,
    },
}

/// Sink for events. A bounded tokio channel keeps a slow UI from stalling the
/// transfer while still applying back-pressure if it falls far behind.
pub type EventSender = tokio::sync::mpsc::Sender<Event>;
/// Receiving half of the event channel.
pub type EventReceiver = tokio::sync::mpsc::Receiver<Event>;

/// Create an event channel with a sensible buffer.
pub fn channel() -> (EventSender, EventReceiver) {
    tokio::sync::mpsc::channel(256)
}

/// Send an event, ignoring a closed receiver (the UI went away; the transfer
/// should keep going regardless).
pub(crate) async fn emit(tx: &EventSender, event: Event) {
    let _ = tx.send(event).await;
}
