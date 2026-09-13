//! Tickets and links.
//!
//! A ticket is iroh-blobs' [`BlobTicket`]: the sender's endpoint id, how to
//! reach it, and the BLAKE3 hash of the collection. A link is the same ticket
//! carried in the fragment of a URL (`https://host/#<ticket>`). The fragment is
//! never sent to the web server, so a link host learns nothing about the
//! share. Receivers accept either form.

use std::str::FromStr;

use anyhow::{Context, Result, bail};
use iroh_blobs::ticket::BlobTicket;
use url::Url;

/// Build a link from a ticket and a base URL such as `https://share.example`.
pub fn to_link(base: &str, ticket: &BlobTicket) -> Result<String> {
    let mut url = Url::parse(base).with_context(|| format!("invalid link base {base:?}"))?;
    url.set_fragment(Some(&ticket.to_string()));
    Ok(url.to_string())
}

/// Parse a bare ticket or a link into a ticket.
pub fn parse(input: &str) -> Result<BlobTicket> {
    let input = input.trim();
    if let Ok(ticket) = BlobTicket::from_str(input) {
        return Ok(ticket);
    }
    if let Ok(url) = Url::parse(input) {
        if let Some(fragment) = url.fragment() {
            if let Ok(ticket) = BlobTicket::from_str(fragment) {
                return Ok(ticket);
            }
        }
        // Also accept `https://host/<ticket>` in case a link was rewritten.
        if let Some(last) = url.path_segments().and_then(|mut s| s.next_back()) {
            if let Ok(ticket) = BlobTicket::from_str(last) {
                return Ok(ticket);
            }
        }
        bail!("that link does not contain a share ticket");
    }
    bail!("not a share ticket or link");
}

/// True if `input` is plausibly a ticket or link rather than a file path.
/// Used by the CLI to decide between sending and receiving when given a single
/// positional argument.
pub fn looks_like_ticket(input: &str) -> bool {
    parse(input).is_ok()
}
