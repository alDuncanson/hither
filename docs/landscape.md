# Transport landscape: how file transfer works and how others do it

Reference for architecture decisions. Written 2026-09-13. Companion to
`NOTES.md`.

## 1. The problem in one paragraph

Two devices on ordinary home or office networks both sit behind NAT, and
neither can accept an unsolicited inbound connection. Every file transfer tool
therefore solves the same three sub-problems: **rendezvous** (how the two
sides find each other), **connectivity** (how bytes cross the NATs and
firewalls between them), and **trust** (how the receiver knows the bytes are
what the sender meant and that nobody else read them). Cloud drives "solve"
connectivity by turning one peer-to-peer connection into two client-to-server
connections, at the price of parking your data on someone else's disk and
making you wait for the upload before the download can start.

## 2. Building blocks

**TCP.** A reliable, ordered byte stream. Outbound connections pass through
almost every NAT and firewall, which is why the whole web works. Inbound
connections do not, and TCP hole punching (both sides connecting at once) is
unreliable. One stream per connection, so a lost packet stalls everything
behind it. `HTTPS` is HTTP over TLS over TCP (HTTP/1.1 and HTTP/2). Port 443
is the one port every corporate proxy allows, though it may inspect the TLS.

**UDP.** Unreliable datagrams with no connection state. That is exactly why
NAT hole punching works over UDP: a NAT creates a mapping when a host sends
outbound, and if both sides send to each other's mapping at the same moment,
both NATs see "expected" traffic and let it through. STUN-style probes tell a
host what its public mapping looks like. UDP is also what zero-trust and
corporate proxies most often drop, because they cannot inspect it.

**QUIC.** A transport built on UDP with TLS 1.3 built in, many independent
streams per connection (a lost packet only stalls its own stream), connection
migration (change Wi-Fi to cellular mid-transfer without dropping), and
0-RTT reconnects. HTTP/3 is HTTP over QUIC. iroh's direct connections are
QUIC. By design QUIC has no TCP mode: if UDP is blocked, there is no QUIC.
Corporate networks block UDP 443 deliberately so browsers fall back to
HTTP/2, where the proxy can inspect traffic.

**NAT traversal.** The standard toolkit is STUN (learn your public mapping),
hole punching (simultaneous send), TURN (a relay for when punching fails) and
ICE (the algorithm that tries candidate paths in order). WebRTC uses those
names. iroh does the same job with its own pieces: address discovery through
its net report and QUIC address discovery, hole punching coordinated over the
relay connection, and the relay itself as the fallback path. n0 quotes about
90% direct success; Tailscale reports similar with the same design. The 10%
are symmetric NATs, carrier-grade NAT on both ends, and networks that drop
UDP outright.

**Relays.** TURN (WebRTC), DERP (Tailscale), iroh-relay, the magic-wormhole
transit relay and croc's relay are all the same idea: a server that forwards
encrypted bytes it cannot read, so that two peers who both can only dial out
still meet in the middle. Unlike a cloud drive a relay stores nothing and
needs no account. The costs are bandwidth and a central dependency. iroh's
relay speaks WebSocket over HTTPS on 443, which is why it worked through
Zscaler on the work Mac when nothing else did.

**WebRTC.** The only way a browser can do peer-to-peer. It bundles ICE with
STUN and TURN, DTLS for encryption, and SCTP data channels for arbitrary
bytes. It needs a signalling channel (any server) to exchange offers and
answers first. Browser-to-browser works everywhere today. Browser-to-native
requires the native side to embed a WebRTC stack, which is large. Data
channel throughput is acceptable, not great, and TURN fallback is just a
relay with extra steps. WebTorrent is BitTorrent over WebRTC data channels.

**WebSocket.** A bidirectional TCP channel that starts life as an HTTP
request, so proxies treat it as web traffic. Browsers use it to reach iroh
relays; so do native iroh endpoints.

**WebTransport.** Streams and datagrams over HTTP/3 for browsers. With
`serverCertificateHashes` a page could in principle talk QUIC to a peer that
has a self-signed certificate, which is the path n0 mentions for future
direct browser connections. Still UDP, so it changes nothing on networks that
block UDP.

**Content addressing and verified streaming.** Name data by the hash of its
contents. BitTorrent did per-piece hashes in 2001. iroh-blobs uses a BLAKE3
hash tree and the bao encoding to verify every 16 KiB chunk as it arrives
against a single 32-byte root hash, which gives resume for free (you know
exactly which chunks you have) and lets any holder of the data serve it.

**Secrets in the URL fragment.** Everything after `#` never leaves the
browser, so a link can carry a secret without the host learning it.
wormhole.app, Bitwarden Send and the old Firefox Send put an AES key there
to decrypt ciphertext stored on their servers. Our links put an iroh ticket
there: a capability to connect to a specific public key plus a hash to
verify against. Ours is encryption in transit to a known peer; there is no
ciphertext at rest because nothing rests. If we add a keeper node that does
store data, we should encrypt at rest with a key in the fragment too, so the
keeper stays zero-knowledge.

**PAKE.** Password-authenticated key exchange, SPAKE2 in magic-wormhole and
croc. Turns a short human code like `7-crossover-clockwork` into a strong
shared key while giving an attacker exactly one guess. It is the trick that
makes short, speakable codes safe. Our tickets are long and unguessable
instead, which is fine for links and QR codes and worse for reading aloud.

## 3. How the others do it

| Tool | Rendezvous | Data path | Sender can leave? | Who can read |
|---|---|---|---|---|
| Dropbox, OneDrive, Google Drive | account, server | client to server to client, stored indefinitely, zip built on demand | yes | the provider |
| WeTransfer | web upload, emailed link | server, stored about a week | yes | the provider |
| wormhole.app | room id in URL, AES key in fragment | up to 5 GB: encrypted upload to Backblaze, kept 24 h; above 5 GB: WebRTC/WebTorrent peer to peer, tab must stay open; receiver pulls from server and peers at once | yes for the stored tier | nobody |
| magic-wormhole (Python; Rust port exists) | short code, SPAKE2, a "mailbox" message server | direct TCP if the address hints work, else a TCP transit relay; no UDP, no hole punching | no | nobody |
| croc | short code, PAKE | always through a relay (TCP), resumable | no | nobody |
| Snapdrop, PairDrop | same public IP or room code, WebSocket signalling | WebRTC on the local network | no | nobody |
| FilePizza, instant.io | link | WebTorrent in the browser | no | nobody |
| AirDrop, Quick Share | Bluetooth discovery | peer-to-peer Wi-Fi, local only | no | nobody |
| Syncthing, Resilio | device ids, discovery servers | direct with relay fallback, continuous folder sync | n/a, it is sync | nobody |
| Tailscale Taildrop | same tailnet | WireGuard over UDP, DERP relay over TCP 443 when blocked | no | nobody |
| Firefox Send (dead 2020), Thunderbird Send (self-hostable revival) | link with key in fragment | encrypted upload, time-limited | yes | nobody |
| sendme (n0) | ticket | iroh: QUIC direct, relay fallback, verified streaming | no | nobody |
| BitTorrent | infohash, DHT, trackers | swarm, content addressed, unencrypted by default | if seeders exist | anyone in the swarm |

Patterns worth noticing:

- Everyone who avoids storage requires both sides online. Everyone who
  allows the sender to leave stores bytes somewhere. There is no third way;
  the design question is only *whose* disk and *who holds the key*.
- wormhole.app is the most thoughtful hybrid: zero-knowledge storage for
  small transfers, peer-to-peer for large ones, and the client pulls from
  both at once. Its limits are the browser's: WebRTC only, tab must stay
  open above 5 GB, files zipped for download.
- magic-wormhole and croc solved rendezvous and trust beautifully with PAKE
  codes and then punted on connectivity by relaying over TCP. That is why
  they are reliable and why they are slow when direct fails.
- Firefox Send died of abuse: anonymous zero-knowledge storage is a malware
  distribution service unless you gate it. Any keeper node we run must not be
  open to the public.
- AirDrop is the UX bar. It is also local-only and single-vendor, which is
  the gap everything else is trying to fill.

## 4. Why this is still hard in 2026

- NAT is everywhere and IPv6 did not make inbound reachability normal;
  carrier-grade NAT on mobile made it worse.
- Browsers got exactly one peer-to-peer primitive, WebRTC, designed for
  video calls. Nothing simpler ever shipped.
- Incentives point at storage. Retention and accounts are what people pay
  for; relays cost money with nothing to bill; anonymous hosted transfer
  attracts abuse. Dropbox was built in 2007 to sync a folder across your own
  machines and then followed the revenue into teams and enterprise. Sending
  large things to someone without an account was never its core problem, so
  its sharing UX is an afterthought rather than a betrayal.
- Corporate networks block UDP on purpose, so the fastest transport is
  unavailable exactly where many people work.
- The both-online requirement is a real cost. The cloud's asynchrony is a
  real feature, not just lock-in.

## 5. The options for us, with the transport facts applied

**A. Native peers over QUIC with owned relays (the current core).** Direct
QUIC when hole punching works, our own iroh relay over WSS 443 when it does
not. Works everywhere web traffic works, at relay speed on UDP-blocked
networks. Browser receivers are relay-only and need iroh-blobs in wasm,
which does not exist yet.

**B. WebRTC for browsers.** Gives browser-to-browser today. Browser-to-native
needs a WebRTC stack in the app. On corporate networks TURN over TCP 443 is
just a relay again, so it buys nothing there. Worth it only if two browsers
talking directly becomes a priority.

**C. An inbox or keeper node with an HTTPS front door.** A node we own that
accepts announces over iroh from native clients, and can also accept a plain
resumable HTTPS upload (tus-style) and serve plain HTTPS downloads. Works
from any browser and any network that allows web traffic, needs no install on
the sending side, and still verifies with BLAKE3 because the node hashes what
lands. Storage returns, but on our box, encrypted at rest with the key in the
link if we want zero knowledge. Must be invite-only.

**Recommended shape.** A is the core and the fast path. C is the answer to
async, to corporate networks, and to browser senders, all at once, and it is
where the "friend drops files into my inbox" flow naturally lives. Skip B
unless a concrete need appears.

## 6. What would set ours apart

- **Whole trees, verified, resumable.** Hundreds of files as a directory,
  no zip step, every chunk checked, interruptions resume exactly. wormhole.app
  and WeTransfer zip; croc and magic-wormhole move one file or a tarball.
- **Receiver-initiated inbox.** Hand out one stable link; people drop things
  in; you approve. Nobody outside AirDrop's proximity model does this well.
- **One link, one core, every surface.** The same ticket format opens in a
  CLI, a menu bar app, a phone, or a web page that hands off to whichever of
  those is present.
- **Honest about the path.** Show direct versus relayed and the speed, tell
  people when their network is the limit, never make a relayed transfer look
  like a bug.
- **No third-party storage by default, and self-owned when there is any.**
