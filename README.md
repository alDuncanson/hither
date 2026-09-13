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
hither scans/ --code           # also print four words to say over the phone...
hither able-cactus-river-mouse # ...which the other side types instead of the link
hither inbox                   # open your inbox; hand out its link
hither to <inbox-link> scans/  # offer files to someone's inbox
hither friends add sam <link>  # save an inbox under a name...
hither sam scans/              # ...and offer files to it by name
hither id                      # your stable identity (created on first use)
hither id export               # the secret behind it, as 24 words, to move machines
hither doctor                  # can this network do direct connections?
hither upgrade                 # replace this binary with the newest release
```

## Install

```sh
curl -fsSL https://alduncanson.github.io/hither/install.sh | sh
```

Or skip the separate step entirely. `run.sh` installs hither if it is
missing and then runs whatever follows, so a link's page can show one
command:

```sh
curl -fsSL https://alduncanson.github.io/hither/run.sh | sh -s -- <ticket>
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

Links are the same ticket in a URL fragment, `https://host/#<ticket>`, so
the landing page never sees which share was opened. `hither://<ticket>` is
the same ticket again, for the desktop app's URL scheme.

### Spoken codes

`hither scans/ --code` also prints four words such as
`able-cactus-river-mouse`. The other side runs `hither able cactus river
mouse` and gets the files. No server is involved: both sides derive the same
keypair from the words, the sender runs a tiny second endpoint under that
key that hands over the real ticket, and the receiver finds it through
iroh's normal discovery. Four words are 44 bits; a code lives only while the
sender's window is open. Two-word codes would need a PAKE and a rendezvous
server, which is a possible later addition.

### Moving your identity

`hither id export` prints your secret as 24 words (or `--hex`), plus your
inbox token. `hither id import <words> --token <hex>` on another machine
makes it you, with the same inbox link. The words carry a checksum, so a
mistyped word is caught rather than silently producing a different identity.
Store the export like a password: anyone holding it can act as you.

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

Between machines you own, save the inbox once with `hither friends add
laptop <link>` and from then on `hither laptop photos/` is the whole command.

The link carries your endpoint id, your relay, and a 128-bit token. It is
permission to *offer* you files, not to write them: the prompt is what
protects your disk. Anyone who has the link can knock.

Both sides must be online during the transfer; the sender's command exits
when your inbox confirms it has everything. See `docs/architecture.md` for
the keeper node that removes the both-online requirement later.

## Desktop app (alpha)

`apps/desktop` is a Tauri v2 menu bar app over the same core: drop files or
folders on it and the link is on your clipboard; paste a link, ticket or
four words to receive; open your inbox and approve offers as notifications
arrive; keep friends; show recovery words. It registers the `hither://`
scheme, so the landing page's "open in the hither app" link works once the
app is installed. The front end is plain HTML and JavaScript with no build
step; the Rust side is one file of commands that call `hither-core`.

```sh
cargo install tauri-cli --version "^2" --locked
cd apps/desktop/src-tauri && cargo tauri dev        # run it
cd apps/desktop/src-tauri && cargo tauri build      # hither.app and a .dmg
```

It is a separate Cargo project on purpose, so the workspace build stays lean
and Linux CI needs no WebKit. Signing and notarization are wired through
Tauri's usual environment variables once a Developer ID is available.

## Repository layout

```
crates/core   hither-core   UI-agnostic library: net, Sender::start, receive, Inbox, Event stream, e2e tests
crates/cli    hither-cli    the `hither` binary: clap + indicatif over the core
apps/desktop  hither-desktop  Tauri menu bar app (own Cargo project); ui/ is the front end
site/         the landing page served at alduncanson.github.io/hither
install.sh    the curl | sh installer
run.sh        install-if-missing, then run: the one-command form the landing page shows
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
cargo test            # unit tests plus end-to-end transfers over loopback
cargo clippy --all-targets -- -D warnings
cargo run -- some/folder
```

The end-to-end tests in `crates/core/tests/e2e.rs` start real endpoints on
`127.0.0.1` with relays and discovery off, so they need no network and pass
on machines whose LAN UDP is blocked. `rust-toolchain.toml` lists `clippy`
and `rustfmt`, so rustup installs them on first use.

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
