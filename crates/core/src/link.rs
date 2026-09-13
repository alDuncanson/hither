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

use crate::inbox::InboxTicket;

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
        if let Some(fragment) = url.fragment()
            && let Ok(ticket) = BlobTicket::from_str(fragment)
        {
            return Ok(ticket);
        }
        // Also accept `https://host/<ticket>` in case a link was rewritten.
        if let Some(last) = url.path_segments().and_then(|mut s| s.next_back())
            && let Ok(ticket) = BlobTicket::from_str(last)
        {
            return Ok(ticket);
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

/// Either kind of thing a link can carry.
#[derive(Debug, Clone)]
pub enum Link {
    /// Pull these files from the sender.
    Share(BlobTicket),
    /// Offer files to this inbox.
    Inbox(InboxTicket),
}

/// Parse a bare ticket or link of either kind.
pub fn parse_any(input: &str) -> Result<Link> {
    let input = input.trim();
    for c in candidate_strings(input) {
        if let Ok(t) = BlobTicket::from_str(&c) {
            return Ok(Link::Share(t));
        }
        if let Ok(t) = InboxTicket::from_str(&c) {
            return Ok(Link::Inbox(t));
        }
    }
    if Url::parse(input).is_ok() {
        bail!("that link does not contain a hither ticket");
    }
    bail!("not a hither ticket or link")
}

/// Parse an inbox ticket or link.
pub fn parse_inbox(input: &str) -> Result<InboxTicket> {
    match parse_any(input)? {
        Link::Inbox(t) => Ok(t),
        Link::Share(_) => {
            bail!("that is a share link, not an inbox link. Use `hither <link>` to receive it")
        }
    }
}

/// Build an inbox link from a ticket and a base URL.
pub fn inbox_to_link(base: &str, ticket: &InboxTicket) -> Result<String> {
    let mut url = Url::parse(base).with_context(|| format!("invalid link base {base:?}"))?;
    url.set_fragment(Some(&ticket.to_string()));
    Ok(url.to_string())
}

/// The strings inside `input` that might be a ticket: the input itself, a
/// URL fragment, and a URL's last path segment.
fn candidate_strings(input: &str) -> Vec<String> {
    let mut out = vec![input.to_string()];
    if let Ok(url) = Url::parse(input) {
        if let Some(f) = url.fragment() {
            out.push(f.to_string());
        }
        if let Some(last) = url.path_segments().and_then(|mut s| s.next_back()) {
            out.push(last.to_string());
        }
    }
    out
}
