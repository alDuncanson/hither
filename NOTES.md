# Notes: pick up here

Last session: 2026-09-12, on Al's work Mac. Continue on the personal machine.

## Where things stand

The first pass of the CLI works and was verified end to end (see README for
usage). Two crates:

- `crates/core` (`hither-core`): `Sender::start`, `receive`, one serialisable
  `Event` stream, no UI deps. This is the piece every front end shares.
- `crates/cli` (binary `hither`): clap + indicatif over the core.

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

**Decided 2026-09-13: `hither`.** Binary `hither`, crates `hither-core` and
`hither-cli`, repo github.com/alDuncanson/hither. Theme: second-millennium
English, used with a light hand in copy (the sender's waiting line is "Hie
thee hither"; inbox offers can be "tidings") while commands stay plain.
Portage was the runner-up. The research that led here follows.

`share` was the placeholder for the binary, crates and repo. Criteria: one short
word, comfortable to type twice a day, reads naturally as both `hither photos/`
and `hither inbox`, sayable over a phone call and spellable after hearing it,
not an existing command, free on Homebrew, no trademark clash for a Mac app,
no "box" or "drive" connotations. crates.io single words are all squatted;
publish as `hither-cli` and `hither-core` with the binary `hither` (ripgrep ships
as `rg`, nobody minds).

Checked 2026-09-13 (crates.io API, formulae.brew.sh, GitHub user, `command -v`,
DNS NS lookup as a weak domain signal; "maybe" means no NS record, confirm at
a registrar):

| name | reads as | crates | brew | .dev / .app / .sh | verdict |
|---|---|---|---|---|---|
| **chute** | `chute photos/` · `chute inbox` · "drop it in my chute" | taken (use chute-cli) | free | maybe / taken / taken | **first choice.** A mail chute is a slot you drop things into that delivers them elsewhere: fits both flows, one syllable, five letters. |
| **hither** | `hither photos/` · "send it hither" | **free** | free | maybe / taken / maybe | **distinctive alternative.** Archaic, charming, unmistakable. Reads better for receiving than sending. |
| posthaste | `posthaste photos/` | free | free | taken / taken / maybe | perfect meaning (with all speed, from mail riders); nine letters is a lot to type. |
| ginseng | `ginseng photos/` | free | free | taken / taken / maybe | keep-the-brand option; no semantic link, but neither had Dropbox. |
| spool | `spool photos/` · `spool inbox` | taken | free | maybe / taken / taken | a spool is a delivery queue; printing connotation. |
| tote | `tote photos/` | taken | free | all taken | short, carries things; UK betting brand. |
| beam | `beam photos/` · "beam it to me" | taken | free | all taken | best verb, worst discoverability (Apache Beam, many Beams). |
| haul, ferry, toss, sling, lob, whisk | verbs | taken | free | all taken | fine words, nothing free around them. |
| airmail | `airmail inbox` | free | free | all taken | ideal semantics, but Airmail is an established macOS/iOS mail client. No. |
| handoff, duffel, pigeon, sprocket | | | | | Apple feature, travel API company, Flutter tool, Homebrew formula. No. |
| sendreel, chutepost, tubepost, sendhither | coinages | free | free | mostly maybe | everything free, but they read like startups. Fallback only. |

Before committing to one: search USPTO TDSR/TESS for software (classes 9 and
42), confirm the domain at a registrar, and grab the Homebrew tap name.

Archaic and whimsical candidates (Al liked `hither`), checked the same way:

| name | meaning | sending | receiving | crates | brew | domains |
|---|---|---|---|---|---|---|
| **hither** | to here | "send it hither" | `hither <link>` brings it here | free | free | .dev/.sh maybe |
| **wend** | to make one's way | `wend photos/` sends them on their way | `wend inbox` | free | free | all taken |
| **forthwith** | immediately | `forthwith photos/` | `forthwith inbox` | free | free | .sh maybe |
| **apace** | swiftly | `apace photos/` | `apace inbox` | free | free | .sh maybe |
| **tidings** | news brought to you | "send tidings" | `tidings inbox` is lovely | free | free | all taken |
| **waybill** | the document that travels with a shipment | `waybill photos/` | `waybill inbox` | free | free | .sh maybe |
| **consign** | hand over for delivery | `consign photos/` | `consign inbox` | free | free | .sh maybe |
| **portage** | carrying a boat between two waters | `portage photos/` (across the NAT) | `portage inbox` | free | free | all taken |
| pouch | the diplomatic pouch | `pouch photos/` | `pouch inbox` | free | free | all taken |
| thither / hence / yonder | to there / from here / over there | send-only words | weak for inbox | free | free | mixed |
| hie | hasten ("hie thee hither") | `hie photos/` | pairs with hither as a verb | free | free | .sh maybe |
| prithee | please, I pray thee | jokey | jokey | free | free | all maybe |
| missive, proffer, beckon, valise, dray, lading, impart, convey, herald, envoy, summon, remit, trove | good words | | | taken | free | mostly taken |
| dak | Indian English for the post | short, obscure | | free | free | all taken |

Pairing idea if the binary is `hither`: keep the commands plain (`hither photos/`,
`hither <link>`, `hither inbox`) and spend the whimsy on copy, e.g. the sender's
waiting line "Hie thee hither" and the inbox's "tidings" for offers.


## The two flows

### `hither <paths>`: sender-initiated (built)

"Here, take this." Sender hashes, serves, prints a ticket; receiver pulls.
Ephemeral identity per run. Good for one-to-many and for "I made this".
Weakness for the film use case: all the work lands on the friend, who must
run the app and stay online.

### `hither inbox`: receiver-initiated (next)

The motivated party runs a long-lived receiver with a stable identity and hands
out one link. The friend opens it, drops files in, and is done. The receiver
sees an accept prompt (file list and size) before anything downloads.

Design, kept deliberately close to what exists:

1. **Identity.** Persist a `SecretKey` at the platform data dir
   (`~/Library/Application Support/hither/identity` on macOS, XDG elsewhere).
   `hither inbox` always uses it, so the endpoint id, and therefore the link,
   stays stable. Store a random 128-bit inbox token beside it; `hither inbox
   --rotate` replaces the token and invalidates old links.
2. **Link.** `https://host/#inbox:<endpoint-id>:<token>`, optionally with
   the relay URL and direct addrs appended the way a `BlobTicket` does. With
   only the id, receivers rely on n0's DNS discovery, which the N0 preset
   already publishes to.
3. **Announce protocol.** Custom ALPN, e.g. `hither/inbox/0`, registered on the
   inbox's router next to iroh-blobs. The sender side does what `hither <paths>`
   does today (`Sender::start`) and then opens one bidirectional stream to the
   inbox and sends a length-prefixed, postcard-encoded
   `Announce { token, ticket, files: Vec<FileEntry>, sender_label }`. The
   inbox compares the token in constant time, emits `Event::Offer { .. }`,
   and waits for the UI to call `accept` or `decline`. It answers
   `Accept` or `Decline { reason }` on the same stream.
4. **Transfer.** On accept the inbox calls the existing `receive()` with the
   announced ticket into `<inbox dir>/<sender-label or short id>-<date>/`.
   When it finishes it writes `Done` on the announce stream so the sender's
   CLI (`hither to <inbox-link> <paths>`) can exit. If the inbox dies mid-way
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
`docs/landscape.md`; the design decisions, both flows, connectivity ladder,
link anatomy, data model, friction map, UX principles, deployment topology,
phases and open decisions are in `docs/architecture.md` (with diagrams).

## The zero-account model (why no sign-up is ever needed)

Four jobs a file transfer needs, and which primitive does each one here:

| Job | Primitive | What it gives the person |
|---|---|---|
| "Did I get the right bytes?" (integrity) | BLAKE3 content addressing: the link names the collection by its 32-byte root hash; every 16 KiB chunk is verified against that root as it arrives | Correct by construction, resume for free, any holder can serve, no server's word to trust |
| "Am I allowed to fetch this?" (authorization) | The hash is 256 bits and unguessable, and iroh-blobs only serves what you ask for by hash | Holding the link *is* the permission; nothing to log in to. Treat links like the files |
| "Am I talking to the right machine?" (authentication) | An endpoint id is a public key; dialing it means TLS pins that key | No certificate authority, no account server, no impersonation by the network |
| "Can anyone else read it?" (confidentiality) | QUIC/TLS 1.3 between the two endpoints; relays forward ciphertext | Nothing at rest anywhere, so nothing to encrypt at rest (until a keeper exists) |

Consequences for friction: identity is created locally and silently
(`hither id`), authorization is link possession, and the only thing a person
ever does is open a link and run two commands. Persisting the identity file
(iroh's "persistent identity") is what makes inbox links stable, lets an
inbox remember senders, and lets iroh's DNS discovery map an id to its
current addresses. Cost: a private key file to keep; losing it means a new id
and new inbox link, and there is deliberately no password or recovery flow.
Add "back up / restore identity" (copy the file, or export as words) before
anyone relies on an inbox link.

**PAKE is for a different channel.** magic-wormhole and croc solve "the only
channel is a human voice": a two-word code (~16 bits) plus SPAKE2 yields a
strong key, a wrong guess fails visibly and burns the code, and the
rendezvous server sees nothing it can brute-force. Links do not need it (256
bits of hash is self-sufficient). We would want it only for a spoken-code
mode: `hither` prints `7-crossover-clockwork`, the other side types it, and a
tiny rendezvous service matches the two by code and hands over the ticket
encrypted under the PAKE key. That is the "small web application" worth
building if short or spoken codes matter: a magic-wormhole-style mailbox
server, a few hundred lines, zero knowledge. Short links (`hither.link/x7k2`)
fit the same service: it stores the ticket encrypted with a key that stays in
the URL fragment.

**Profiles.** Today the sender's `--as` label is a plain string, socially
fine, cryptographically nothing. Later: a signed profile record (name,
avatar hash) published under the id the way iroh publishes address records
via pkarr/DNS, so anyone can resolve id -> name without a server of ours;
avatars fetched as blobs. An inbox's `--accept-from` list is the seed of a
local address book (id -> name, "always accept").

**Where a web service would still help, and what it must never hold:** short
and spoken codes (rendezvous), profile lookup if DNS records prove too small,
push wake-ups for a future mobile inbox, and the keeper. None of them should
ever hold plaintext files or private keys.

Release note: bump the workspace `version` in `Cargo.toml` before tagging so
`hither --version` matches the tag (v0.1.0-alpha.1 binaries report 0.1.0),
then run `cargo build` so `Cargo.lock` records the new version, or every
`--locked` job in CI fails (alpha.7 learned this the hard way).

## Short links and spoken codes (open decision)

Pain today: a share or inbox ticket is 140-210 characters, so moving it
between two machines you own means emailing yourself. Fixes, cheapest first:

1. **Friends** (done 2026-09-13): `hither friends add laptop <inbox link>`
   once, then `hither laptop photos/`. Solves the repeated two-machine case
   with no server. `hither inbox` prints the save-once hint.
2. **Serverless short codes**, no PAKE: `hither photos/ --code` prints four
   words (about 50 bits from a 7776-word list). Both sides derive a keypair
   from the words; the sender runs a second, short-lived endpoint under it
   that hands over the real ticket on a tiny ALPN; the receiver derives the
   same endpoint id and dials it through n0's DNS discovery. Zero
   infrastructure. Weakness: anyone can check whether a code is live with one
   DNS lookup, so codes must be long (four words) and short-lived (minutes).
3. **Two-word codes with PAKE**, magic-wormhole style: needs a rendezvous
   server we run (a few hundred lines, zero knowledge, could live on
   alduncanson.com). SPAKE2 makes each guess cost one interactive round with
   the real peer, so two words are enough. Also the natural home for short
   URLs (`hither.link/x7k2` with the key in the fragment).

Recommendation: 1 now (done), 2 for the alpha if spoken codes matter before
the VPS exists, 3 when the VPS is up; 2 and 3 can share the same CLI surface
(`--code`, `hither <words>`).

## Code hygiene (done 2026-09-13, before more features)

- `crates/core/tests/e2e.rs`: six end-to-end tests over loopback (roundtrip,
  refuse-overwrite, cancel-then-resume, inbox accept-all, inbox ask yes/no,
  wrong token). `NetOptions::local()` is what makes them run anywhere.
- `net.rs` is the single place endpoints are configured; every option struct
  carries a `NetOptions`.
- `receive.rs` and `inbox.rs` are split into named phases; each file starts
  with a module comment that lists them. Section banners (`// ---- name ----`)
  mark the parts of the longer files; rustdoc (`///`, `//!`) carries the
  explanations. That is normal Rust practice: doc comments are the primary
  tool, banners are optional and only worth it in files over a few hundred
  lines.
- clippy is part of the toolchain file and CI fails on warnings.
- `hither doctor` now says to click Allow if macOS showed the dialog.

## Distribution without app stores (decided 2026-09-13)

Kap is the model: a macOS menu bar app shipped as a `.dmg` on GitHub releases
plus `brew install --cask kap`, never the App Store. The same works for us:

- **Signing and notarization**, not the App Store, is what makes a downloaded
  app open without Gatekeeper's "cannot be opened" dialog. It needs the paid
  Apple Developer Program (USD 99/year) for a Developer ID certificate;
  Tauri v2 signs and notarizes in CI from a few secrets
  (`APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_ID`,
  `APPLE_PASSWORD`, `APPLE_TEAM_ID`, or an App Store Connect API key). No
  review, no sandbox requirements, ship whenever.
- The same Developer ID can sign the CLI binary in `release.yml`, which
  also removes the "accept incoming connections?" firewall prompt: macOS
  auto-allows signed software by default.
- **Homebrew tap** for both: `brew install alduncanson/tap/hither` for the
  CLI and `brew install --cask alduncanson/tap/hither` for the app, from a
  small `homebrew-tap` repo whose formulas point at the GitHub release
  assets. Getting into homebrew-core/cask proper needs notability; the tap
  needs nothing.
- **Updates**: Tauri's updater plugin checks a signed manifest on GitHub
  releases and updates the app in place; the CLI can grow `hither upgrade`
  that re-runs the installer.
- Until the developer account exists, unsigned builds still work: users
  right-click, Open, or `xattr -dr com.apple.quarantine`. Fine for the alpha
  audience, not for friends.

## First real transfer (2026-09-13)

Al's personal Mac to his girlfriend's Mac: `hither <image>`, she opened the
link, pasted the one command into Terminal, the image arrived. Two findings:

- Terminal warned about the pasted command. That is macOS's general caution
  for text pasted from a website into a shell; the landing page now says so
  and links the scripts. The real fix for non-developers is the desktop app
  reached from the same link, or a signed `.pkg`/cask, not the terminal.
- Files should land in Downloads, not in whatever folder the command ran
  from. Fixed in alpha.6: `hither <ticket>` and `hither inbox` default to
  the OS Downloads folder; `--out`/`--dir` still override.

## To do on the personal machine: signing and distribution

Al's Apple Developer Program membership is active (auto-renew). Everything
below needs the account's certificates and secrets, which live there.

1. **Developer ID Application certificate.** In Xcode or developer.apple.com
   create one if none exists; export it as a `.p12` with a password. Note
   the Team ID (membership page).
2. **App Store Connect API key** (preferred over an Apple ID password for
   notarization): Users and Access -> Integrations -> App Store Connect API,
   role Developer. Download the `.p8` once; note Key ID and Issuer ID.
3. **GitHub secrets** on alDuncanson/hither: `APPLE_CERTIFICATE` (base64 of
   the .p12), `APPLE_CERTIFICATE_PASSWORD`,
   `APPLE_SIGNING_IDENTITY` ("Developer ID Application: Al Duncanson (TEAMID)"),
   `APPLE_TEAM_ID`, `APPLE_API_KEY` (Key ID), `APPLE_API_ISSUER`,
   `APPLE_API_KEY_CONTENT` (base64 of the .p8), and `KEYCHAIN_PASSWORD`
   (any random string for the CI keychain).
4. **Turn on the desktop release job.** `.github/workflows/desktop-release.yml`
   already exists (tauri-action, universal build, sign, notarize, staple,
   attach to the tag's release). It is gated on the repository variable
   `DESKTOP_RELEASES`; set it to `true` (Settings -> Secrets and variables ->
   Actions -> Variables) after the secrets, then push a tag. Bump
   `apps/desktop/src-tauri/Cargo.toml` and `tauri.conf.json` versions to
   match the tag first.
5. **Sign the CLI too** in `release.yml` for the two macOS targets:
   `codesign --sign "Developer ID Application: ..." --options runtime
   --timestamp hither`, then zip and `xcrun notarytool submit --wait` with
   the API key, so the firewall prompt disappears. Re-tar after signing.
6. **Homebrew tap.** Create `alDuncanson/homebrew-tap` with a formula for
   the CLI (url = release tarball per arch, sha256 from the `.sha256` files)
   and a cask for the app (dmg). Then `brew install alDuncanson/tap/hither`
   and `brew install --cask alDuncanson/tap/hither`. Add a step to
   `release.yml` that bumps the formula, or do it by hand at first.
7. **Updater.** `tauri-plugin-updater` with a signing keypair
   (`cargo tauri signer generate`), pubkey in `tauri.conf.json`, the
   `latest.json` manifest published with each release.
8. **Test the app on screen**: `cd apps/desktop/src-tauri && cargo tauri dev`.
   Check: tray icon, drop a folder, link on clipboard, paste a link in
   Receive, open the inbox and accept an offer from the CLI, `hither://`
   from the landing page opens the app.

## Auto-update decision (2026-09-13)

`run.sh` (the one-line friend path) now compares the installed version with
the newest release on every run and reinstalls when they differ, falling
back to the installed copy when GitHub is unreachable; `HITHER_NO_UPDATE=1`
skips it. Rationale: that path exists for people who never want to think
about versions, the installer verifies the release's sha256 so trust is the
same as the first install, and our protocols are versioned by ALPN so a
version skew between sender and receiver fails cleanly rather than
silently. A plain `hither` invocation does *not* self-update: developers
expect their binaries to stay put, and `hither upgrade` exists. The desktop
app should get Tauri's updater plugin, which asks before installing, the
convention for GUI apps.

## Distribution for non-developers (decided 2026-09-13)

Homebrew is a developer tool; it is convenience for us, not the path for
friends. The path for everyone else is the signed and notarized `.dmg` of
the desktop app, downloaded from the landing page: open, drag to
Applications, done, and from then on `hither://` links open in it. Until
signing is set up, the terminal one-liner is the interim path, Apple's paste
warning included. An unsigned `.dmg` is never published: Gatekeeper refuses
it outright on current macOS, which is a worse experience than the terminal.

## Known upstream alert

Dependabot flags `glib 0.18.5` (unsound iterator impl, fixed in 0.20) in
`apps/desktop/src-tauri/Cargo.lock`. It reaches us through
tauri -> tray-icon -> libappindicator -> gtk 0.18, a Linux-only path that
never compiles into the macOS app, and no semver-compatible update exists
until Tauri moves to gtk 0.20. Nothing to do on our side; re-check after the
next Tauri minor.

## UX self-audit (2026-09-13, alpha.7)

Walked every screen a person sees. Fixed now:

- **The link lands on the sender's clipboard** (`pbcopy`, `wl-copy`, `xclip`
  or `xsel`, whichever exists; `HITHER_NO_CLIPBOARD=1` disables). The very
  next thing a sender does is paste the link into a message.
- **Spoken codes are on by default** (`--no-code` to skip). Zero cost to the
  sender, and "or say: hither retire shoe crunch amount" is the friendliest
  line we print. A code that fails to start no longer fails the share.
- **`--once`** stops sharing as soon as one receiver has everything, for the
  common one-to-one case. Default stays multi-use.
- **Receives never refuse a collision any more.** A second download of
  `album` lands in `album-2`, like a browser; existing files are untouched.
  `Finished` and `Received` carry `into`, the top-level names written, and
  the CLI prints `Saved 3 files to ~/Downloads/album-2`.
- **Error chains are deduplicated** (`timed out: timed out: timed out` is
  gone) via `net::brief`.
- **The landing page speaks to phones and Windows** instead of showing a
  command that cannot work there.
- **Windows compiles in CI** as a first step toward a Windows build.

Backlog, roughly by value:

- Windows release target and a PowerShell installer once CI is green there.
- A once-a-day "a newer hither is available" hint for direct CLI users
  (`run.sh` already updates; `hither upgrade` exists).
- Inbox codes: `hither <words> <files>`.
- Show the file list before download on the receive side (the manifest
  arrives first; today the CLI only shows counts).
- Progress ETA on long transfers.
- `hither friends add` straight from an inbox link pasted into `hither`
  alone (currently a hint).
- Menu bar app: test on screen, then the updater, then signing.
- Accessibility pass on the CLI colors (all states also carry a word).

## Lessons that cost real time

- **Ctrl-C hang in `hither inbox` (fixed in 0.1.0-alpha.3).** The UI task held
  a `Decider`, the `Decider` held the whole inbox state, and that state held
  an event sender. After shutdown the main task waited for the UI, the UI
  waited for the event channel to close, and the channel was held open by the
  UI's own handle. Tokio had claimed the Ctrl-C handler, so further presses
  did nothing. Rules now in the code: a UI may hold only what it needs to
  answer (`Decider` holds just the pending-offer table); `finish_ui` never
  waits more than two seconds for a renderer; a second Ctrl-C always exits.
  If a command ever ignores Ctrl-C again, look for who still holds an
  `EventSender`.

## Done 2026-09-13 (evening), alpha.5

- `hither id export` / `import`: 24 words (BIP-39 list, MIT) with a BLAKE3
  checksum byte, or 64 hex; inbox token travels with `--token`; `--force`
  to replace a different identity.
- `hither upgrade`: re-runs the published installer into the binary's own
  folder.
- Spoken codes, serverless: `--code` mints four words; `Code::secret_key`
  derives a keypair from them (`blake3::derive_key`, context
  "hither spoken code v0"); `CodeServer` is a second endpoint under that key
  on ALPN `hither/code/0` handing out the ticket; `code::redeem` dials the id
  through discovery with retries. Verified live over n0 DNS + relay in 5 s.
  Inbox codes not yet (`hither <words> <files>`).
- `hither://<ticket>` parses (for the app's URL scheme); the landing page has
  an "open in the hither app" link that only works once an app registers
  the scheme.
- `NetOptions.static_peers` (`.knowing(addr)`) stands in for discovery in
  tests.

## Desktop app (scaffolded 2026-09-13, alpha.5)

`apps/desktop`: Tauri v2, standalone Cargo project (`[workspace]` table keeps
it out of the root workspace), `ui/` is vanilla HTML/JS/CSS served from
`frontendDist` with `withGlobalTauri`, no Node. `src-tauri/src/lib.rs` owns
the tray (template icon, left click toggles the window, right click menu),
hides on close, sets the Accessory activation policy (no Dock icon), and
turns `hither://` deep links into a `hither-open` event.
`src-tauri/src/commands.rs` has one command per core operation and forwards
every core `Event` as `hither-event { source, id, event }`; offers and
completions also raise desktop notifications. Icons: `icons/` generated by
`cargo tauri icon` from an SVG rendered with `qlmanage`; `tray.png` is the
template glyph. Not yet: signing (Al has a Developer ID from Ginseng to
check), a desktop CI job (macOS runner), the updater plugin, a Homebrew tap.
Untested on screen from this session: build it and click through on the
personal machine.

## Next steps, in order

1. Personal machine: clone, build, confirm a **direct** transfer (above).
   Run `hither doctor` first: it should say "Direct connections should work."
2. ~~Pick the name; rename crates, binary, repo; update README.~~ Done 2026-09-13.
3. ~~`identity.rs`, `hither id`, `--identity`.~~ Done 2026-09-13. The inbox
   token is not part of it yet; add it with the inbox.
4. ~~`inbox.rs`: announce protocol, `Inbox`, `send_to`; `hither inbox` and
   `hither to` in the CLI with an accept prompt.~~ Done 2026-09-13. As built:
   ticket kind `inbox` via `iroh-tickets` (prints as `inbox…`, carries id +
   relay + 16-byte token, no direct addrs so it survives address changes);
   ALPN `hither/inbox/0`; one bi stream per offer, frames are u32 LE length +
   postcard; `Announce {token, ticket, files, bytes, label}` then
   `Reply::{Accepted, Declined, Done, Failed}`; token compared in constant
   time, bad token = silent decline (no event); policies Ask / AcceptAll /
   AcceptFrom; offers land in `<dir>/<slug(label or short id)>-<timestamp>/`;
   the pull reuses the inbox's own endpoint (`receive_with`). Verified: auto
   accept, prompted yes, prompted no, stable link across restarts.

   Known flake (both flows, this network): the first connection to a freshly
   started endpoint sometimes times out after 30 s, then the next attempt
   works. Not reproduced under debug logging. Check on the personal machine
   before suspecting the code; if it persists there, capture
   `RUST_LOG=iroh=debug` on both sides.
5. ~~Static landing page~~ Done 2026-09-13 (one-command form via `run.sh`
   added the same day): `site/index.html`, deployed by
   `.github/workflows/pages.yml` to https://alduncanson.github.io/hither/;
   printed links point there by default (`--link-base ""` for ticket only).
   Later: serve it from alduncanson.com and change `DEFAULT_LINK_BASE`. The
   "open in app" hand-off waits for an app that registers a URL scheme.
6. Tauri menu bar app over the same core (drag files in, get a link; inbox
   offers appear as notifications). Then uniffi for mobile.
7. ~~Enable iroh's `platform-verifier` feature; add `hither doctor`; add the
   "via relay" notice.~~ Done 2026-09-13. Doctor uses iroh's
   `unstable-net-report` feature; all of that API lives in `doctor.rs`.
8. Self-hosted `iroh-relay`: **deferred by Al** while the alpha has one user.
   Still wanted before anyone on a managed network relies on it.
   (`--relay URL` already exists; measure relayed throughput when it lands.)
9. ~~CI, release builds~~ Done 2026-09-13: `ci.yml` (fmt/build/test on
   macOS + Linux), `release.yml` (tag `v*` builds 4 targets, publishes a
   GitHub release with sha256 files), `install.sh` (curl | sh, checksum
   verified, installs to ~/.local/bin). **Distribution decision (Al):** no app
   stores; curl-installable binaries for people comfortable with a terminal,
   which is the target audience to start. Still open: clippy in CI, and
   signing/notarization for macOS so the firewall prompt goes away.

## Odds and ends

- `-v` on the sender prints the addresses baked into the ticket. Useful when
  a receiver can't connect.
- Test scripts: extract tickets with `hither blob[a-z0-9]{50,}`; a loose
  `blob[a-z0-9]+` matches "blobs" in log lines. macOS has no `timeout`.
- Reference implementations: n0's `sendme` (same crates, CLI only) and the
  iroh-blobs `transfer-collection` example. iroh docs: relays, discovery,
  browser support (relay-only; blobs not yet on wasm as of Sept 2026).
