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
hither inbox                   # open your inbox; hand out its link
hither to <inbox-link> scans/  # offer files to someone's inbox
hither id                      # your stable identity (created on first use)
hither doctor                  # can this network do direct connections?
```

## Install

```sh
curl -fsSL https://alduncanson.github.io/hither/install.sh | sh
```

That fetches the latest release for your machine from GitHub, checks its
SHA-256, and puts one binary in `~/.local/bin`. `HITHER_INSTALL_DIR` changes
the folder, `HITHER_VERSION=v0.1.0` pins a version. macOS (Apple silicon and
Intel) and Linux (x86_64 and arm64) are built; Windows users can build from
source with `cargo install --path crates/cli` for now.

Every link hither prints opens https://alduncanson.github.io/hither/, which
reads the ticket from the URL fragment and shows the two commands above.
The page is static and the fragment never reaches the server.

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

### Inbox: when you are the one who wants the files

`hither inbox` runs a long-lived receiver under your stable identity and
prints a link that keeps working across restarts. Whoever opens it runs
`hither to <link> <files>` (or just `hither <link> <files>`); their side
hashes the files and *announces* the share to your inbox: file list, sizes,
an optional name. You see the offer and answer y or n before a single payload
byte moves. Accepted offers land in `<dir>/<name>-<timestamp>/` using the
same verified, resumable download. `--accept-all` and `--accept-from <id>`
skip the prompt; `--rotate` replaces the token in the link, which invalidates
every link handed out so far.

The link carries your endpoint id, your relay, and a 128-bit token. It is
permission to *offer* you files, not to write them: the prompt is what
protects your disk. Anyone who has the link can knock.

Both sides must be online during the transfer; the sender's command exits
when your inbox confirms it has everything. See `docs/architecture.md` for
the keeper node that removes the both-online requirement later.

## Repository layout

```
crates/core   hither-core   UI-agnostic library: Sender::start, receive, Inbox, Event stream
crates/cli    hither-cli    the `hither` binary: clap + indicatif over the core
site/         the landing page served at alduncanson.github.io/hither
install.sh    the curl | sh installer
docs/         architecture.md (design, diagrams), landscape.md (transports, prior art)
NOTES.md      where things stand and what is next
```

## Development

Commits follow [Conventional Commits](https://www.conventionalcommits.org).
CI runs fmt, build and tests on macOS and Linux; pushing a `v*` tag builds
release binaries for four targets and publishes them; changes under `site/`
or to `install.sh` redeploy the landing page.

Rust 1.91 or newer is required by iroh 1.x. `rust-toolchain.toml` pins an
installed toolchain.

```sh
cargo build
cargo test
cargo run -- some/folder
```

Set `RUST_LOG` or pass `-v`/`-vv` for logs. `--relay disabled` forces a
direct-only connection, useful on a LAN.

`hither id` stores a keypair at the platform data directory
(`~/Library/Application Support/hither/identity` on macOS). `HITHER_SECRET`
(64 hex characters) overrides it and `HITHER_IDENTITY_FILE` moves it.
`hither <paths> --identity` shares under that identity instead of a fresh one.

`hither doctor` runs a bare UDP self-test, times the relay, and reads iroh's
network report. Exit code 0 means direct connections should work, 2 means
relay-only, 3 means nothing reachable. `--json` prints the full report.

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

## License

MIT or Apache-2.0, at your option.
