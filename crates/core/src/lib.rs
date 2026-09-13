//! # hither-core
//!
//! UI-agnostic peer-to-peer file sharing on top of [iroh](https://iroh.computer)
//! and iroh-blobs. Every front end (CLI, desktop, mobile, web) drives the
//! same operations and renders the same [`Event`] stream:
//!
//! - [`Sender::start`] hashes files and folders, serves them, and yields a
//!   ticket (or link) to hand to the other person.
//! - [`receive`] takes that ticket, downloads with BLAKE3 verification, and
//!   writes the files into a directory. Interrupted downloads resume.
//! - [`Inbox::open`] listens under a persistent [`Identity`] for offers, and
//!   [`send_to`] makes one. See [`inbox`] for the protocol.
//! - [`Code`] turns a ticket into four spoken words and back, without a
//!   server (see [`code`]).
//! - [`diagnose`] explains what this network allows.
//!
//! Module map: [`net`] builds endpoints; [`events`] is the contract with
//! UIs; [`paths`] and [`link`] convert between the outside world and our
//! types; [`identity`] and [`friends`] are the two small files on disk.
//! Tickets are standard iroh-blobs collection tickets, so `sendme receive`
//! can read them too.

pub mod code;
pub mod doctor;
pub mod events;
pub mod friends;
pub mod identity;
pub mod inbox;
pub mod link;
pub mod net;
pub mod paths;
pub mod receive;
pub mod send;
mod throttle;
pub mod words;

pub use code::Code;
pub use doctor::{DoctorOptions, DoctorReport, Verdict, diagnose};
pub use events::{Event, EventReceiver, EventSender, FileEntry, PathKind, channel};
pub use friends::Friends;
pub use identity::Identity;
pub use inbox::{AcceptPolicy, Inbox, InboxOptions, InboxTicket, send_to};
pub use iroh::{EndpointId, RelayMode, SecretKey};
pub use iroh_blobs::ticket::BlobTicket;
pub use net::NetOptions;
pub use receive::{Cancelled, ReceiveOptions, Received, partial_dir, receive, receive_with};
pub use send::{SendOptions, Sender, TicketKind};
pub use tokio_util::sync::CancellationToken;
