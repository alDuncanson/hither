# Notes: pick up here

Last session: 2026-09-12, on Al's work Mac. Continue on the personal machine.

## Where things stand

The first pass of the CLI works and was verified end to end (see README for
usage). Two crates:

- `crates/core` (`share-core`): `Sender::start`, `receive`, one serialisable
  `Event` stream, no UI deps. This is the piece every front end shares.
- `crates/cli` (binary `share`): clap + indicatif over the core.

Verified with two local processes: 119 MiB nested tree byte-identical;
interrupt at 30 MiB then resume; collision refused before any payload moves;
link form (`https://host/#<ticket>`), implicit receive, bad input. Tickets are
plain iroh-blobs collection tickets, so `sendme receive <ticket>` reads them.

**Not verified: direct (hole-punched) connections.** The work Mac is a
managed device running Zscaler (tunnel at 100.64.0.1), CrowdStrike Falcon and
Jamf, and Zscaler drops UDP: a plain socket test to the Mac's own LAN address
succeeds over TCP and times out over UDP. QUIC is UDP, so hole punching can
never succeed there and every transfer fell back to n0's public relay at
~1 MB/s. (An application-firewall rule for the debug binary looked like the
cause at first; copying the binary to a new path produced no prompt and no
change, so it was not.) First thing on the personal machine:

```sh
cargo run -- some/folder --relay disabled        # terminal 1
cargo run -- get <ticket> --relay disabled       # terminal 2
```

Click **Allow** if macOS asks about incoming connections. 120 MB should take a
second or two and the receiver should print "direct". If it still times out,
check UDP itself before suspecting the code: the snippet in the README's
troubleshooting section tells TCP-only networks apart from firewall rules.

Product note from this: corporate and managed machines may be relay-only by
policy. That is an argument for running our own relay eventually, and for the
inbox/keeper design where the always-on node sits on a network we control.

Also seen: intermittent "could not reach the sender" timeouts on ~4 of 12
relayed connection attempts, clustered right after large relayed transfers.
Smells like public-relay rate limiting; unproven. Revisit once direct works.

Toolchain: iroh 1.x needs Rust 1.91+. `rust-toolchain.toml` pins `1.97.0`
because that was installed on the work Mac; on the personal machine either
install it or change the channel to `stable`.

## Naming

`share` is a placeholder for the binary, crates and repo. Criteria: one short
word, comfortable to type twice a day, reads naturally as both `NAME photos/`
and `NAME inbox`, not an existing common command, available on crates.io,
Homebrew and as a domain, no "box" or "drive" connotations.

Candidates jotted down, none checked for availability yet:

| name | reads as | notes |
|---|---|---|
| `beam` | `beam photos/`, `beam inbox` | direct, fast, sci-fi. Check conflicts (Apache Beam is a library, not a CLI). |
| `hand` / `pass` | `hand photos/` | plain verbs; `pass` is taken (password manager). |
| `toss` / `sling` | `toss photos/` | casual; sling reads oddly for inbox. |
| `ferry` | `ferry photos/` | carries things across; unhurried connotation. |
| `courier` | `courier photos/` | descriptive, long. |
| `pigeon` | `pigeon photos/` | memorable; carrier pigeon; a bit jokey. |
| `parcel` / `pouch` | `parcel inbox` | noun-first; okay for a GUI app name. |
| `ginseng` | | the previous attempt's name; free to reuse. |

Avoid `relay` (means something specific in iroh) and `drop`/`box`.

## The two flows

### `NAME <paths>`: sender-initiated (built)

"Here, take this." Sender hashes, serves, prints a ticket; receiver pulls.
Ephemeral identity per run. Good for one-to-many and for "I made this".
Weakness for the film use case: all the work lands on the friend, who must
run the app and stay online.

### `NAME inbox`: receiver-initiated (next)

The motivated party runs a long-lived receiver with a stable identity and hands
out one link. The friend opens it, drops files in, and is done. The receiver
sees an accept prompt (file list and size) before anything downloads.

Design, kept deliberately close to what exists:

1. **Identity.** Persist a `SecretKey` at the platform data dir
   (`~/Library/Application Support/NAME/identity` on macOS, XDG elsewhere).
   `NAME inbox` always uses it, so the endpoint id, and therefore the link,
   stays stable. Store a random 128-bit inbox token beside it; `NAME inbox
   --rotate` replaces the token and invalidates old links.
2. **Link.** `https://host/#inbox:<endpoint-id>:<token>`, optionally with
   the relay URL and direct addrs appended the way a `BlobTicket` does. With
   only the id, receivers rely on n0's DNS discovery, which the N0 preset
   already publishes to.
3. **Announce protocol.** Custom ALPN, e.g. `NAME/inbox/0`, registered on the
   inbox's router next to iroh-blobs. The sender side does what `NAME <paths>`
   does today (`Sender::start`) and then opens one bidirectional stream to the
   inbox and sends a length-prefixed, postcard-encoded
   `Announce { token, ticket, files: Vec<FileEntry>, sender_label }`. The
   inbox compares the token in constant time, emits `Event::Offer { .. }`,
   and waits for the UI to call `accept` or `decline`. It answers
   `Accept` or `Decline { reason }` on the same stream.
4. **Transfer.** On accept the inbox calls the existing `receive()` with the
   announced ticket into `<inbox dir>/<sender-label or short id>-<date>/`.
   When it finishes it writes `Done` on the announce stream so the sender's
   CLI (`NAME to <inbox-link> <paths>`) can exit. If the inbox dies mid-way
   the sender keeps serving; the inbox resumes on restart because the
   partial store is content-addressed.
5. **Why announce-then-pull instead of iroh-blobs `Push`.** It reuses the
   verified download path unchanged, the receiver decides before bytes move,
   and the blobs protocol needs no access control. `Push` stays an option for
   a browser sender later, but browsers are blocked on iroh-blobs wasm anyway.
6. **Security.** The token is a capability to *offer* files, not to write
   them; the accept prompt (or an allow-list of sender ids for auto-accept)
   is what protects disk space. `paths::destination` already rejects `..`
   and separators in names. Add a per-offer size cap and a "remember this
   sender" option.
7. **Later.** The inbox is the natural home for the always-on "keeper": run
   it on a Pi or VPS, auto-accept from known senders, and have the laptop
   pull from it. That is how the "friend uploads at 11pm, I fetch tomorrow"
   case gets solved without any third-party storage.

Core API sketch:

```rust
let inbox = Inbox::open(identity, InboxOptions { dir, auto_accept, .. }, events).await?;
inbox.link();                       // stable, printable, QR-able
// event loop: Event::Offer { id, sender, files, bytes } -> inbox.accept(id) / inbox.decline(id)
send_to(inbox_link, paths, SendOptions, events).await?;   // Sender::start + announce + wait for Done
```

New events: `InboxReady { link }`, `Offer { id, sender, files, bytes }`,
`OfferAccepted { id }`, `OfferDeclined { id, reason }`, `OfferDone { id }`.

## Corporate networks and UDP

Found on the work Mac: Zscaler drops UDP, so QUIC direct paths are impossible
there and only the relay (WebSocket over HTTPS, port 443) gets through. QUIC
has no TCP mode, so this cannot be fixed on the client side, and we should
never try to evade a corporate policy. The goal is a relay path good enough
that users on such networks do not care. Options, roughly by value:

1. **Own the relay.** `iroh-relay` on a small VPS. The ~1 MB/s we saw is
   n0's free-tier rate limit, not a property of relaying; a self-hosted relay
   runs at the box's uplink. n0 dedicated relays are USD 199/month/region.
   This also serves browsers, which are relay-only regardless.
2. **Co-locate the relay with the inbox/keeper node.** Corporate sender ->
   WSS 443 -> relay -> node on the same box. Direct-to-server speed, no third
   party. Makes the inbox the natural answer for managed machines.
3. **Trust the OS root store.** iroh-relay verifies TLS with bundled webpki
   roots by default; the `platform-verifier` cargo feature uses the system
   store, which is what survives TLS inspection with a corporate CA. One-line
   change; do it regardless.
4. **Detect and say so.** iroh's net report can tell us UDP is blocked. A
   `doctor` subcommand plus a one-line notice ("your network blocks direct
   connections, transferring via relay") turns a mystery into an expected
   mode. First cheap step.
5. **HTTPS upload into the keeper as the last-resort path.** Resumable
   (tus-style), works from any browser and any web-allowed network, no
   install for the sender, still BLAKE3-verified because the keeper hashes
   what lands. Reintroduces storage, but ours. Also sidesteps the
   iroh-blobs-in-wasm blocker for browser senders. Invite-only.
6. **TCP direct transport plugin** (`Endpoint::add_custom_transport`).
   Buildable, low value: needs inbound TCP on one side, and zero-trust
   proxies usually allow only 80/443 outbound.
7. **IT allowlisting.** Only relevant if this becomes a sanctioned workplace
   tool.

Recommended: 1 + 2 + 3 as the strategy, 4 immediately, 5 later as the
async/browser/corporate fallback. Background on the transports (TCP, UDP,
QUIC, WebRTC, relays, PAKE) and how other tools handle this is in
`docs/landscape.md`.

## Next steps, in order

1. Personal machine: clone, build, confirm a **direct** transfer (above).
2. Pick the name; rename crates, binary, repo; update README.
3. `identity.rs` in core (load-or-create secret key, token), `NAME id` to
   print the endpoint id, `--identity` flag so `NAME <paths>` can be stable too.
4. `inbox.rs`: announce protocol, `Inbox`, `send_to`; `NAME inbox` and
   `NAME to` in the CLI with an accept prompt.
5. Static landing page that reads the fragment and offers "open in app" or
   "get the app". Nothing about the share ever reaches the host.
6. Tauri menu bar app over the same core (drag files in, get a link; inbox
   offers appear as notifications). Then uniffi for mobile.
7. Enable iroh's `platform-verifier` feature; add `NAME doctor` (net report:
   UDP blocked? relay reachable? direct addrs) and the "via relay" notice.
8. Stand up a self-hosted `iroh-relay` on a VPS and point the CLI at it
   (`--relay URL` already exists); measure relayed throughput.
9. Housekeeping: CI (fmt, test, build matrix), clippy, signed and notarized
   release builds so macOS never shows the firewall prompt.

## Odds and ends

- `-v` on the sender prints the addresses baked into the ticket. Useful when
  a receiver can't connect.
- Test scripts: extract tickets with `share get blob[a-z0-9]{50,}`; a loose
  `blob[a-z0-9]+` matches "blobs" in log lines. macOS has no `timeout`.
- Reference implementations: n0's `sendme` (same crates, CLI only) and the
  iroh-blobs `transfer-collection` example. iroh docs: relays, discovery,
  browser support (relay-only; blobs not yet on wasm as of Sept 2026).
