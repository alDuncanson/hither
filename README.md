# share

Send files and folders directly to someone, peer to peer. No upload, no zip,
no cloud account. The other side gets a ticket (or a link) and pulls the files
straight from you, verified byte for byte on the way in.

```sh
share scans/                 # share a folder
share img1.jpg img2.tiff     # share a few files
share get <ticket-or-link>   # receive into the current directory
share <ticket-or-link>       # same thing, shorter
```

Built on [iroh](https://iroh.computer) 1.x and iroh-blobs. Tickets are
standard iroh-blobs collection tickets, so `sendme receive <ticket>` works
too.

> Working name. The binary, crates and repo can be renamed later.

## Why

Sharing a few gigabytes of photos today means uploading to a cloud drive,
letting it build a zip on the fly, downloading that zip over a single fragile
HTTP stream, and hoping Archive Utility can open it. This tool replaces that
with a direct, resumable, content-addressed transfer:

- **Verified streaming.** Every 16 KiB chunk is checked against a BLAKE3 hash
  tree as it arrives. A finished download is correct by construction.
- **Resumable.** Interrupt it, run the same command again, it continues from
  the last verified chunk.
- **Direct when possible.** iroh hole-punches a direct QUIC connection about
  90% of the time and falls back to an encrypted relay otherwise.
- **Nothing to overwrite.** The receiver checks for name collisions before it
  moves a single payload byte.

## How it works

`share <paths>` hashes the files in place (nothing is copied), builds a
collection, starts an iroh endpoint and prints a ticket. The ticket holds the
sender's public endpoint id, how to reach it, and the collection hash. Anyone
holding the ticket can download; the connection is end-to-end encrypted.

`share get <ticket>` connects, fetches the tiny collection index first (so it
can show the file list and refuse collisions), then downloads what is missing
into a `.share-partial-<hash>` directory next to the destination and moves the
verified files into place.

Links are the same ticket in a URL fragment, `https://host/#<ticket>`, so a
future landing page never sees which share was opened. Use `--link-base` to
print that form.

The sender must stay online until the other side has everything. See the
roadmap for what changes that.

## Layout

```
crates/core   share-core   UI-agnostic library: Sender::start, receive, Event stream
crates/cli    share-cli    the `share` binary: clap + indicatif over the core
```

`share-core` has no terminal or UI dependencies. Every front end consumes the
same serialisable `Event` stream, which is the seam for the desktop, mobile
and web clients.

## Roadmap

1. **Link landing page.** A static page that reads the ticket from the URL
   fragment and hands off to an installed app, or explains how to get one.
2. **Mailbox / inbox.** Invert the flow: the person who *wants* the files runs
   a long-lived receiver with a stable identity and hands out a link. The
   sender opens it, drops files in, and the receiver approves and pulls using
   the same verified path. Requires persisted identity and a small
   announce protocol.
3. **Keeper node.** An optional always-on node that holds a share so the
   sender can go offline. Content-addressed, self-hosted, verified.
4. **Desktop (menu bar) and mobile.** Same core via Tauri and uniffi.
5. **Web.** Blocked on iroh-blobs compiling to wasm; browsers are relay-only.

## Development

Rust 1.91 or newer is required by iroh 1.x. `rust-toolchain.toml` pins an
installed toolchain.

```sh
cargo build
cargo test
cargo run -- some/folder
```

Set `RUST_LOG` or pass `-v`/`-vv` for logs. `--relay disabled` forces a
direct-only connection, useful on a LAN.

## Troubleshooting

**Everything says "relayed" on macOS, or direct-only transfers time out.**
The macOS application firewall blocks incoming connections for unsigned
binaries such as a local debug build, so hole punching never completes and
iroh falls back to the relay. Allow the binary once:

```sh
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add "$PWD/target/debug/share"
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp "$PWD/target/debug/share"
```

Signed and notarized release builds will not need this. Check current rules
with `socketfilterfw --listapps`.

**n0's public relays are slow (about 1 MB/s).** They are rate limited and
meant for development. A production deployment runs its own relay or uses a
paid n0 plan; direct connections are not affected.
