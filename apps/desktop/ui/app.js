// hither desktop, front end. Plain JS over Tauri's global API: no bundler.
// Every button calls a Rust command; progress arrives as `hither-event`
// events shaped { source, id, event } where `event` is a core Event.

const T = window.__TAURI__ || {};
const invoke = T.core?.invoke ?? (() => Promise.reject(new Error("not running inside hither")));
const listen = T.event?.listen ?? (() => Promise.resolve(() => {}));

const $ = (sel, root = document) => root.querySelector(sel);
const el = (tag, attrs = {}, ...children) => {
  const n = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") n.className = v;
    else if (k.startsWith("on")) n.addEventListener(k.slice(2), v);
    else if (v !== null && v !== undefined) n.setAttribute(k, v);
  }
  for (const c of children) n.append(c instanceof Node ? c : document.createTextNode(String(c)));
  return n;
};
const bytes = (n) => {
  const u = ["B", "KiB", "MiB", "GiB", "TiB"];
  let v = Number(n), i = 0;
  while (v >= 1024 && i < u.length - 1) { v /= 1024; i++; }
  return i === 0 ? `${n} B` : `${v.toFixed(v >= 100 ? 0 : 1)} ${u[i]}`;
};
const files = (n) => (n === 1 ? "1 file" : `${n} files`);

let toastTimer;
function toast(msg) {
  const t = $("#toast");
  t.textContent = msg; t.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => { t.hidden = true; }, 1800);
}
async function copy(text) {
  try {
    if (T.clipboardManager?.writeText) await T.clipboardManager.writeText(text);
    else await invoke("plugin:clipboard-manager|write_text", { text });
    toast("Copied");
  } catch (e) { toast("Could not copy"); }
}
async function pick(opts) {
  if (T.dialog?.open) return T.dialog.open(opts);
  return invoke("plugin:dialog|open", { options: opts });
}
async function reveal(path) {
  try {
    if (T.opener?.revealItemInDir) await T.opener.revealItemInDir(path);
    else await invoke("plugin:opener|reveal_item_in_dir", { path });
  } catch (e) { toast("Could not open Finder"); }
}

// ------------------------------------------------------------------ tabs
function showTab(name) {
  document.querySelectorAll(".tab").forEach((b) => b.classList.toggle("is-active", b.dataset.tab === name));
  document.querySelectorAll(".panel").forEach((p) => p.classList.toggle("is-active", p.id === `tab-${name}`));
}
document.querySelectorAll(".tab").forEach((b) => b.addEventListener("click", () => showTab(b.dataset.tab)));

// ----------------------------------------------------------------- share
const shares = new Map(); // id -> { card, peers: Map<connection, {bar, done, reqs}>, total }

function shareCard(id, paths) {
  const card = el("div", { class: "card", "data-share": id });
  const title = el("div", { class: "card-title" }, paths.length === 1 ? paths[0].split("/").pop() : `${paths.length} items`);
  const sub = el("div", { class: "card-sub" }, "Hashing…");
  const bar = el("div", { class: "bar" }, el("i"));
  const body = el("div", { class: "cards" });
  const actions = el("div", { class: "actions" });
  card.append(el("div", { class: "card-head" }, el("div", {}, title, sub), actions), bar, body);
  $("#shares").prepend(card);
  return { card, sub, bar, body, actions, peers: new Map(), total: 0, hashed: {}, sizes: {}, hashedDone: 0 };
}

async function startShare(paths) {
  if (!paths || paths.length === 0) return;
  const code = $("#opt-code").checked;
  const pending = shareCard("pending", paths);
  try {
    const info = await invoke("share", { paths, code });
    pending.card.dataset.share = info.id;
    shares.set(info.id, pending);
    pending.total = info.bytes;
    pending.sub.textContent = `Sharing ${files(info.files)} (${bytes(info.bytes)}). Keep hither running until they have everything.`;
    pending.bar.remove();
    const link = info.link || info.ticket;
    pending.body.append(el("div", { class: "mono link" }, link));
    pending.actions.append(el("button", { class: "secondary", onclick: () => copy(link) }, "Copy link"));
    if (info.code) {
      pending.body.append(el("div", { class: "card-sub" }, "or say: ", el("b", {}, `hither ${info.code}`)));
      pending.actions.append(el("button", { class: "secondary", onclick: () => copy(`hither ${info.code}`) }, "Copy words"));
    }
    pending.actions.append(el("button", { class: "secondary danger", onclick: async () => { await invoke("stop_share", { id: info.id }); shares.delete(info.id); pending.card.remove(); } }, "Stop"));
    copy(link);
  } catch (e) {
    pending.sub.textContent = `Could not share: ${e}`;
    pending.sub.classList.add("bad");
    pending.bar.remove();
    pending.actions.append(el("button", { class: "secondary", onclick: () => pending.card.remove() }, "Dismiss"));
  }
}

function onShareEvent(id, ev) {
  const s = shares.get(id) || [...shares.values()].find((x) => x.card.dataset.share === "pending");
  if (!s) return;
  switch (ev.type) {
    case "import_started": s.total = ev.bytes; break;
    case "import_file_started": s.sizes[ev.name] = ev.size; break;
    case "import_file_progress": s.hashed[ev.name] = ev.offset; redrawHashing(s); break;
    case "import_file_done": delete s.hashed[ev.name]; s.hashedDone += s.sizes[ev.name] || 0; redrawHashing(s); break;
    case "peer_connected": {
      const row = el("div", { class: "peer" }, el("span", {}, `${ev.peer || "someone"} connected`), el("div", { class: "bar" }, el("i")));
      s.body.append(row);
      s.peers.set(ev.connection, { row, label: ev.peer || "someone", done: 0, reqs: new Map() });
      break;
    }
    case "upload_started": {
      const p = s.peers.get(ev.connection); if (!p) break;
      const r = p.reqs.get(ev.request) || { done: 0, offset: 0 };
      r.done += r.offset; r.offset = 0; p.reqs.set(ev.request, r);
      if (ev.name) p.row.firstChild.textContent = `${p.label} · ${ev.name}`;
      redrawPeer(s, p); break;
    }
    case "upload_progress": {
      const p = s.peers.get(ev.connection); if (!p) break;
      const r = p.reqs.get(ev.request); if (r) r.offset = ev.offset;
      redrawPeer(s, p); break;
    }
    case "upload_done": {
      const p = s.peers.get(ev.connection); if (!p) break;
      p.reqs.delete(ev.request); p.done += ev.bytes; redrawPeer(s, p); break;
    }
    case "peer_disconnected": {
      const p = s.peers.get(ev.connection); if (!p) break;
      const pos = Math.max(ev.bytes_sent || 0, peerPosition(s, p));
      p.row.firstChild.textContent = s.total && ev.bytes_sent >= s.total ? `${p.label} received everything` : `${p.label} disconnected after ${bytes(pos)}`;
      p.row.querySelector(".bar").remove();
      s.peers.delete(ev.connection); break;
    }
  }
}
function redrawHashing(s) {
  const inflight = Object.values(s.hashed).reduce((a, b) => a + b, 0);
  const pct = s.total ? Math.min(100, (100 * (s.hashedDone + inflight)) / s.total) : 0;
  const i = s.bar.querySelector("i"); if (i) i.style.width = `${pct}%`;
}
function peerPosition(s, p) {
  let inflight = 0; for (const r of p.reqs.values()) inflight += r.done + r.offset;
  return Math.min(s.total, p.done + inflight);
}
function redrawPeer(s, p) {
  const i = p.row.querySelector(".bar i");
  if (i) i.style.width = `${s.total ? (100 * peerPosition(s, p)) / s.total : 0}%`;
}

// Drag and drop: Tauri delivers real file paths.
(async () => {
  const dz = $("#dropzone");
  try {
    const wv = T.webview?.getCurrentWebview?.();
    if (wv?.onDragDropEvent) {
      await wv.onDragDropEvent((e) => {
        const t = e.payload?.type;
        if (t === "enter" || t === "over") dz.classList.add("is-over");
        else if (t === "leave") dz.classList.remove("is-over");
        else if (t === "drop") { dz.classList.remove("is-over"); showTab("share"); startShare(e.payload.paths || []); }
      });
    }
  } catch (e) { /* no drag-drop outside Tauri */ }
  $("#pick-files").addEventListener("click", async () => { const r = await pick({ multiple: true, directory: false }); if (r) startShare(Array.isArray(r) ? r : [r]); });
  $("#pick-folder").addEventListener("click", async () => { const r = await pick({ multiple: false, directory: true }); if (r) startShare([r]); });
})();

// --------------------------------------------------------------- receive
let recvBusy = false;
async function startReceive(input) {
  if (recvBusy) return;
  input = (input ?? $("#recv-input").value).trim();
  if (!input) return;
  recvBusy = true;
  const status = $("#recv-status"); status.className = "status"; status.textContent = "Connecting…";
  $("#recv-start").disabled = true;
  const bar = el("div", { class: "bar" }, el("i")); status.after(bar);
  try {
    const info = await invoke("receive", { input, outDir: $("#recv-dir").value || null });
    status.className = "status good";
    const where = info.into && info.into.length === 1 ? `${info.dir}/${info.into[0]}` : info.dir;
    status.textContent = `Saved ${files(info.files)} (${bytes(info.bytes)}) to ${where}`;
    status.append(" ", el("button", { class: "linkish", onclick: () => reveal(info.dir) }, "Show in Finder"));
  } catch (e) {
    status.className = "status bad"; status.textContent = String(e);
  } finally {
    bar.remove(); recvBusy = false; $("#recv-start").disabled = false;
  }
}
function onReceiveEvent(ev, statusEl) {
  const status = statusEl || $("#recv-status");
  const bar = status.nextElementSibling?.classList?.contains("bar") ? status.nextElementSibling.querySelector("i") : null;
  switch (ev.type) {
    case "connected": status.textContent = `Connected to ${ev.peer}. Fetching the file list…`; break;
    case "manifest_received": status.textContent = `Receiving ${files(ev.files.length)} (${bytes(ev.bytes)})${ev.have ? ` · resuming, ${bytes(ev.have)} already here` : ""}`; break;
    case "download_progress": if (bar && ev.total) bar.style.width = `${(100 * ev.bytes) / ev.total}%`; break;
    case "path_changed": if (ev.kind === "relay") status.textContent += " (via relay)"; break;
    case "download_done": status.textContent = "Verified. Writing files…"; break;
  }
}
$("#recv-start").addEventListener("click", () => startReceive());
$("#recv-input").addEventListener("keydown", (e) => { if (e.key === "Enter") startReceive(); });
$("#recv-pick-dir").addEventListener("click", async () => { const r = await pick({ directory: true, multiple: false }); if (r) $("#recv-dir").value = r; });
invoke("default_dir").then((d) => { $("#recv-dir").value = d; }).catch(() => {});

// ----------------------------------------------------------------- inbox
let inboxOpen = false;
const offers = new Map(); // id -> { card, status }
async function toggleInbox() {
  const btn = $("#inbox-toggle"); btn.disabled = true;
  try {
    if (!inboxOpen) {
      const info = await invoke("inbox_open", { dir: null, acceptAll: $("#inbox-accept-all").checked });
      inboxOpen = true;
      $("#inbox-link").textContent = info.link || info.ticket;
      $("#inbox-dir").textContent = info.dir;
      $("#inbox-open").hidden = false;
      $("#inbox-state").textContent = "Open. Anyone holding this link can offer you files.";
      btn.textContent = "Close inbox";
      $("#inbox-copy").onclick = () => copy(info.link || info.ticket);
    } else {
      await invoke("inbox_close");
      inboxOpen = false;
      $("#inbox-open").hidden = true;
      $("#inbox-state").textContent = "Closed. Open it to hand out a link people can drop files into.";
      btn.textContent = "Open inbox";
    }
  } catch (e) { toast(String(e)); } finally { btn.disabled = false; }
}
$("#inbox-toggle").addEventListener("click", toggleInbox);

function onInboxEvent(ev) {
  switch (ev.type) {
    case "offer": {
      const who = ev.label ? `${ev.label} (${ev.from_short})` : ev.from_short;
      const list = el("div", { class: "card-sub mono" }, ev.files.slice(0, 6).map((f) => f.name).join("\n") + (ev.files.length > 6 ? `\n… and ${ev.files.length - 6} more` : ""));
      list.style.whiteSpace = "pre-wrap";
      const status = el("div", { class: "status" });
      const actions = el("div", { class: "actions" });
      const card = el("div", { class: "card" },
        el("div", { class: "card-title" }, `Tidings from ${who}: ${files(ev.files.length)} (${bytes(ev.bytes)})`),
        list, actions, status);
      if (ev.pending) {
        actions.append(
          el("button", { class: "primary", onclick: async () => { await invoke("inbox_decide", { id: ev.id, accept: true }); actions.remove(); } }, "Accept"),
          el("button", { class: "secondary", onclick: async () => { await invoke("inbox_decide", { id: ev.id, accept: false }); actions.remove(); } }, "Decline"));
      }
      $("#offers").prepend(card);
      offers.set(ev.id, { card, status, actions });
      break;
    }
    case "offer_accepted": { const o = offers.get(ev.id); if (o) { o.actions.remove(); o.status.textContent = "Accepted. Bringing them hither…"; o.status.after(el("div", { class: "bar" }, el("i"))); } break; }
    case "offer_declined": { const o = offers.get(ev.id); if (o) { o.actions.remove(); o.status.textContent = "Declined."; } break; }
    case "offer_done": { const o = offers.get(ev.id); if (o) { o.card.querySelector(".bar")?.remove(); o.status.className = "status good"; o.status.textContent = `Saved ${files(ev.files)} (${bytes(ev.bytes)}) to ${ev.dir}`; o.status.append(" ", el("button", { class: "linkish", onclick: () => reveal(ev.dir) }, "Show in Finder")); } break; }
    case "offer_failed": { const o = offers.get(ev.id); if (o) { o.card.querySelector(".bar")?.remove(); o.status.className = "status bad"; o.status.textContent = ev.reason; } break; }
    default: {
      // Receive-side progress for whichever offer is currently downloading.
      const active = [...offers.values()].find((o) => o.card.querySelector(".bar"));
      if (active) onReceiveEvent(ev, active.status);
    }
  }
}

// --------------------------------------------------------------- friends
async function loadFriends() {
  const list = $("#friends"); list.textContent = "";
  try {
    const all = await invoke("friends");
    if (all.length === 0) list.append(el("div", { class: "card-sub" }, "No friends saved yet."));
    for (const f of all) {
      list.append(el("div", { class: "friend" },
        el("span", {}, el("b", {}, f.name), " ", el("span", { class: "mono card-sub" }, f.short_id)),
        el("span", { class: "actions" },
          el("button", { class: "secondary", onclick: () => sendToFriend(f.name) }, "Send files…"),
          el("button", { class: "secondary danger", onclick: async () => { await invoke("friend_remove", { name: f.name }); loadFriends(); } }, "Forget"))));
    }
  } catch (e) { list.append(el("div", { class: "status bad" }, String(e))); }
}
$("#friend-add").addEventListener("click", async () => {
  try {
    await invoke("friend_add", { name: $("#friend-name").value.trim(), link: $("#friend-link").value.trim() });
    $("#friend-name").value = ""; $("#friend-link").value = ""; loadFriends(); toast("Saved");
  } catch (e) { toast(String(e)); }
});
async function sendToFriend(target) {
  const r = await pick({ multiple: true, directory: false }); if (!r) return;
  const paths = Array.isArray(r) ? r : [r];
  const status = $("#to-status"); status.className = "status"; status.textContent = `Offering ${files(paths.length)} to ${target}…`;
  try {
    const d = await invoke("send_to", { target, paths, label: null });
    status.className = "status good"; status.textContent = `Delivered ${files(d.files)} (${bytes(d.bytes)}). They have everything.`;
  } catch (e) { status.className = "status bad"; status.textContent = String(e); }
}
function onToEvent(ev) {
  const status = $("#to-status");
  if (ev.type === "offer_sent") status.textContent = `Offered. Waiting for ${ev.to} to accept…`;
  if (ev.type === "to_accepted") status.textContent = "They accepted. Sending…";
  if (ev.type === "to_declined") { status.className = "status bad"; status.textContent = "They declined."; }
}
loadFriends();

// -------------------------------------------------------------- settings
invoke("identity").then((id) => { $("#identity-id").textContent = id.endpoint_id; }).catch((e) => { $("#identity-id").textContent = String(e); });
$("#identity-export").addEventListener("click", async () => {
  try {
    const words = await invoke("identity_export", { hex: false });
    const box = $("#identity-words");
    box.textContent = words; box.hidden = false;
    box.before(el("div", { class: "status bad" }, "This is your private key. Anyone holding it can act as you. Store it like a password."));
    $("#identity-copy").hidden = false; $("#identity-copy").onclick = () => copy(words);
    $("#identity-export").hidden = true;
  } catch (e) { toast(String(e)); }
});
$("#doctor-run").addEventListener("click", async () => {
  const out = $("#doctor-out"); out.className = "status"; out.textContent = "Checking…"; $("#doctor-run").disabled = true;
  try {
    const r = await invoke("doctor");
    const verdict = { direct_likely: "Direct connections should work.", relay_only: "Transfers will go via relay on this network.", offline: "Nothing can connect from here right now." }[r.verdict] || r.verdict;
    out.className = `status ${r.verdict === "direct_likely" ? "good" : r.verdict === "offline" ? "bad" : ""}`;
    out.textContent = `${verdict}\n${r.reasons.map((x) => "· " + x).join("\n")}`;
  } catch (e) { out.className = "status bad"; out.textContent = String(e); } finally { $("#doctor-run").disabled = false; }
});

// ---------------------------------------------------------------- events
listen("hither-event", ({ payload }) => {
  const { source, id, event } = payload;
  if (source === "share") onShareEvent(id, event);
  else if (source === "receive") onReceiveEvent(event);
  else if (source === "inbox") onInboxEvent(event);
  else if (source === "to") onToEvent(event);
});
// hither://<ticket> opened from the landing page.
listen("hither-open", ({ payload }) => {
  if (payload.kind === "share") { showTab("receive"); $("#recv-input").value = payload.ticket; startReceive(payload.ticket); }
  else if (payload.kind === "inbox") { showTab("inbox"); $("#friend-link").value = payload.ticket; $("#friend-name").focus(); toast("Someone's inbox link: save it as a friend to send to them"); }
  else toast(payload.error || "That link is not a hither link");
});
