# hither

*Bring files here, directly.*

Send files and folders to someone with no upload, no zip and no account. The
other side gets a ticket or a link and pulls the files straight from you,
verified byte for byte as they arrive.

```sh
hither scans/                  # share a folder
hither img1.jpg img2.tiff      # share a few files
hither <ticket-or-link>        # bring a share hither, into the current directory
hither get <ticket-or-link>    # the same, spelled out
```

Built on [iroh](https://iroh.computer) 1.x and iroh-blobs. Tickets are
standard iroh-blobs collection tickets, so `sendme receive <ticket>` reads
them too.

## Why

Sharing a few gigabytes of photos today means uploading to a cloud drive,
letting it build a zip on the fly, downloading that zip over one fragile HTTP
stream, and hoping Archive Utility can open it. hither replaces that with a
direct, resumable, content-addressed transfer:

- **Verified streaming.** Every 16 KiB chunk is checked against a BLAKE3 hash
  tree as it arrives. A finished download is correct by construction.
- **Resumable.** Interrupt it, run the same command again, it continues from
  the last verified chunk.
- **Direct when possible.** iroh hole-punches a direct QUIC connection about
  90% of the time and falls back to an encrypted relay otherwise.
- **Nothing overwritten.** The receiver checks for name collisions before it
  moves a single payload byte.

## How it works

`hither <paths>` hashes the files in place (nothing is copied), builds a
collection, starts an iroh endpoint and prints a ticket. The ticket holds the
sender's public endpoint id, how to reach it, and the collection hash. Anyone
holding the ticket can download; the connection is end-to-end encrypted.

`hither <ticket>` connects, fetches the tiny collection index first (so it
can show the file list and refuse collisions), then downloads what is missing
into a `.hither-partial-<hash>` directory next to the destination and moves
the verified files into place.

Links are the same ticket in a URL fragment, `https://host/#<ticket>`, so a
future landing page never sees which share was opened. Use `--link-base` to
print that form.

The sender must stay online until the other side has everything. See
`NOTES.md` and `docs/architecture.md` for the inbox and keeper designs that
change that.

## Layout

```
crates/core   hither-core   UI-agnostic library: Sender::start, receive, Event stream
crates/cli    hither-cli    the `hither` binary: clap + indicatif over the core
docs/         architecture.md (design, diagrams), landscape.md (transports, prior art)
NOTES.md      where things stand and what is next
```

`hither-core` has no terminal or UI dependencies. Every front end consumes
the same serialisable `Event` stream, which is the seam for the desktop,
mobile and web clients.

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

**Everything says "relayed", or direct-only transfers time out.** iroh's
direct paths are QUIC over UDP. Check whether UDP works at all on the machine
before suspecting anything else:

```sh
python3 - <<'UDPTEST'
import socket, subprocess
ip = subprocess.run(["sh","-c","ipconfig getifaddr en0"],capture_output=True,text=True).stdout.strip()
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.bind((ip, 0)); s.settimeout(2)
c = socket.socket(socket.AF_INET, socket.SOCK_DGRAM); c.sendto(b"x", (ip, s.getsockname()[1]))
try: s.recvfrom(1); print("udp ok")
except Exception as e: print("udp blocked:", e)
UDPTEST
```

If that prints "udp blocked", a VPN or zero-trust client (Zscaler, for
example, on managed work machines) is dropping UDP and only the relay path
can work. Nothing in this tool can change that.

If UDP works but transfers are still relayed, the macOS application firewall
may be blocking incoming connections for the unsigned debug binary. Click
Allow when prompted, or on a machine you administer:

```sh
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --add "$PWD/target/debug/hither"
sudo /usr/libexec/ApplicationFirewall/socketfilterfw --unblockapp "$PWD/target/debug/hither"
```

Signed and notarized release builds will not trigger the prompt.

**n0's public relays are slow (about 1 MB/s).** They are rate limited and
meant for development. A production deployment runs its own relay or uses a
paid n0 plan; direct connections are not affected.
