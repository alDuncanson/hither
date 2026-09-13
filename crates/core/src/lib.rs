//! # hither-core
//!
//! UI-agnostic peer-to-peer file sharing on top of [iroh](https://iroh.computer)
//! and iroh-blobs. Every front end (CLI, desktop, mobile, web) drives the same
//! two operations and renders the same [`Event`] stream:
//!
//! - [`Sender::start`] hashes a set of files and folders, serves them, and
//!   yields a ticket (or link) to hand to the other person.
//! - [`receive`] takes that ticket, downloads with BLAKE3 verification, and
//!   writes the files into a directory. Interrupted downloads resume.
//!
//! Tickets are standard iroh-blobs collection tickets, so `sendme receive`
//! can read them too.

pub mod doctor;
pub mod events;
pub mod identity;
pub mod link;
pub mod paths;
pub mod receive;
pub mod send;
mod throttle;

pub use doctor::{DoctorOptions, DoctorReport, Verdict, diagnose};
pub use events::{Event, EventReceiver, EventSender, FileEntry, PathKind, channel};
pub use identity::Identity;
pub use iroh::RelayMode;
pub use iroh::{EndpointId, SecretKey};
pub use iroh_blobs::ticket::BlobTicket;
pub use receive::{Cancelled, ReceiveOptions, Received, partial_dir, receive};
pub use send::{SendOptions, Sender, TicketKind};
pub use tokio_util::sync::CancellationToken;
