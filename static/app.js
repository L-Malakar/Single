/* Single — dashboard client.
 *
 * Multi-page SPA: Home · Devices · Chats · Activity · Settings (see index.html).
 * The WebSocket protocol is unchanged (see src/protocol.rs):
 *   ← hello {self_identity, service_type}
 *   ← devices {devices:[…]}          live mDNS registry
 *   ← session {session}              P2P session state (handshake steps, rtt…)
 *   ← chat {peer_id, line}           one chat line
 *   ← file {peer_id, event}          transfer progress
 *   ← log {level, msg}               server activity
 *   ← access-state {mode, has_password}   inbound policy (Home › Protocol)
 *   ← connect-request {id, name, …}       Private-mode approval prompt
 *   → ping / refresh / rename / connect {device_id, password?}
 *     disconnect / chat / file-* / set-access {mode, password}
 *     connect-reply {id, accept}
 */

"use strict";

/* keep in sync with Cargo.toml → shown in the sidebar + Settings */
const APP_VERSION = "0.1.3";

const $ = (id) => document.getElementById(id);
const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

/* Monochrome line glyphs — inserted via innerHTML templates. */
const GLYPH = 'fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"';
const ICONS = {
  android: `<svg viewBox="0 0 24 24" ${GLYPH}><path d="M7.1 6.9 5.8 5.2M16.9 6.9l1.3-1.7"/><path d="M5.5 11a6.5 6.5 0 0 1 13 0v4.5a2.5 2.5 0 0 1-2.5 2.5H8a2.5 2.5 0 0 1-2.5-2.5V11z"/><path d="M3.5 12v4M20.5 12v4"/><circle cx="9.6" cy="8.6" r="1" fill="currentColor" stroke="none"/><circle cx="14.4" cy="8.6" r="1" fill="currentColor" stroke="none"/></svg>`,
  windows: `<svg viewBox="0 0 24 24" ${GLYPH}><path d="M3.5 5.4 10.5 4.4V11h-7V5.4z"/><path d="M3.5 13h7v6.6l-7-1V13z"/><path d="M12.5 4.1l8-1.1V11h-8V4.1z"/><path d="M12.5 13h8v7l-8-1.1V13z"/></svg>`,
  linux: `<svg viewBox="0 0 24 24" ${GLYPH}><path d="M5 8l4 4-4 4"/><path d="M12 16.5h7"/></svg>`,
  macos: `<svg viewBox="0 0 384 512" fill="currentColor"><path d="M318.7 268.7c-.2-36.7 16.4-64.4 50-84.8-18.8-26.9-47.2-41.7-84.7-44.6-35.5-2.8-74.3 20.7-88.5 20.7-15 0-49.4-19.7-76.4-19.7C63.3 141.2 4 184.8 4 273.5q0 39.3 14.4 81.2c12.8 36.7 59 126.7 107.2 125.2 25.2-.6 43-17.9 75.8-17.9 31.8 0 48.3 17.9 76.4 17.9 48.6-.7 90.4-82.5 102.6-119.3-65.2-30.7-61.4-90-61.7-91.9zm-52.1-166.8c25.2-30.5 22.9-58.3 22.2-68.2-22.3 1.3-48 15.3-62.7 32.6-16.1 18.3-25.5 41-23.3 66.4 24.2 1.9 46.2-10.6 63.8-30.8z"/></svg>`,
  unknown: `<svg viewBox="0 0 24 24" ${GLYPH}><circle cx="12" cy="12" r="9"/><path d="M9.4 9.6a2.7 2.7 0 1 1 3.7 2.5c-.8.4-1.2 1-1.2 1.9v.3"/><circle cx="12" cy="17.3" r="1" fill="currentColor" stroke="none"/></svg>`,
};
const LABEL = { android: "Android", windows: "Windows", linux: "Linux", macos: "macOS", unknown: "Unknown" };

const state = {
  ws: null,
  self: null,
  serviceType: "_p2pbridge._tcp.local.",
  devices: new Map(),       // id → device dto
  sessions: new Map(),      // peer_id → { dto, unread }
  activePeer: null,
  filter: "all",
  page: "home",
  paused: false,
  logLvl: "all",
  logCount: 0,
  reconnectSec: 1,
  wsPingSent: 0,
  pendingConnect: null,
  /* files bridge — latest snapshot pushed by the server (Files page) */
  bridge: null,
  filesPeer: null,        // peer_id currently browsed
  filesPath: [],          // logical path segments inside the peer's shares
  /* Home › Protocol — inbound connection policy (mirrored on the server) */
  access: { mode: "private", password: "" },
  reqQueue: [],             // pending Private-mode connect-requests
  passFor: null,            // peer awaiting a Protected-mode password
  devicePasswords: {},      // peer_id → last working password (prefill only)
};

/* ================================================================== pages */

function showPage(name) {
  state.page = name;
  document.querySelectorAll(".page").forEach((p) => p.classList.toggle("active", p.id === `page-${name}`));
  document.querySelectorAll("#sideNav .nav-item").forEach((b) => b.classList.toggle("active", b.dataset.page === name));
}

$("sideNav").addEventListener("click", (e) => {
  const btn = e.target.closest(".nav-item");
  if (btn) showPage(btn.dataset.page);
});

/* ============================================================ sidebar collapse
   YouTube-style: the rail shrinks to icons only; the choice persists.
   (≤980 px the rail is always icon-only — see style.css.) */

const SIDE_KEY = "single-side";

function applySide(mini) {
  document.body.classList.toggle("side-mini", !!mini);
  document.documentElement.classList.toggle("side-mini", !!mini);
  /* the whole brand row is the toggle — keep the hover hints truthful */
  const hint = mini ? "Expand menu" : "Collapse menu";
  for (const id of ["sideLogoBtn", "sideNameBtn"]) {
    const el = document.getElementById(id);
    if (el) el.title = hint;
  }
}

function setSide(mini) {
  applySide(mini);
  try { localStorage.setItem(SIDE_KEY, mini ? "mini" : "full"); } catch {}
}

$("sideToggle").addEventListener("click", () => setSide(!document.body.classList.contains("side-mini")));

/* Brand row = second toggle: collapsed, the logo expands the rail (no separate
   extend button); expanded, the logo or the "Single" text collapses it. */
$("sideLogoBtn").addEventListener("click", () => setSide(!document.body.classList.contains("side-mini")));
$("sideNameBtn").addEventListener("click", () => setSide(!document.body.classList.contains("side-mini")));

/* ================================================================== access */

const ACCESS_KEY = "single-access";

function loadAccess() {
  try {
    const saved = JSON.parse(localStorage.getItem(ACCESS_KEY) || "{}");
    if (AccessModes.has(saved.mode)) state.access.mode = saved.mode;
    if (typeof saved.password === "string") state.access.password = saved.password;
  } catch {}
}

function saveAccess() {
  try { localStorage.setItem(ACCESS_KEY, JSON.stringify(state.access)); } catch {}
}

const AccessModes = new Set(["public", "private", "protected"]);

function sendAccess() {
  send({ type: "set-access", mode: state.access.mode, password: state.access.password });
}

function renderAccess() {
  document.querySelectorAll("#protocolSeg .chip").forEach((c) =>
    c.classList.toggle("active", c.dataset.m === state.access.mode));
  const hints = {
    public: "Any device on this Wi-Fi can connect to you directly.",
    private: "Devices must ask for your approval before they can connect.",
    protected: "Any device can connect — but only with your password.",
  };
  const h = hints[state.access.mode] || "";
  $("protoHint").textContent =
    state.access.mode === "protected" && !state.access.password
      ? "Choose a password and press Save to enable Protected mode."
      : h;
  $("protoPassRow").classList.toggle("hidden", state.access.mode !== "protected");
  updateHome();
}

$("protocolSeg").addEventListener("click", (e) => {
  const chip = e.target.closest(".chip");
  if (!chip) return;
  state.access.mode = chip.dataset.m;
  renderAccess();
  if (state.access.mode === "protected" && !state.access.password) {
    $("protoPass").focus();
    return; // wait for a password before telling the server
  }
  sendAccess();
});

function saveProtocolPassword() {
  const pw = $("protoPass").value.trim();
  if (!pw) { $("protoPass").focus(); return; }
  state.access.password = pw;
  saveAccess();
  sendAccess();
  renderAccess();
}
$("protoPassSave").addEventListener("click", saveProtocolPassword);
$("protoPass").addEventListener("keydown", (e) => { if (e.key === "Enter") saveProtocolPassword(); });

/* Server acknowledges (also pushed on every reconnect) */
function applyAccess(msg) {
  if (AccessModes.has(msg.mode)) state.access.mode = msg.mode;
  saveAccess();
  renderAccess();
}

/* ================================================================== home */

$("homeNameBtn").addEventListener("click", () => showPage("settings"));
$("findBtn").addEventListener("click", () => showPage("devices"));

function updateHome() {
  if (!state.self) return;
  $("homeName").textContent = state.self.name;
  const n = state.devices.size;
  const established = [...state.sessions.values()].filter((s) => s.dto.state === "established").length;
  const shares = (state.bridge && state.bridge.shares.length) || 0;
  const bits = [
    `${state.self.ip}:${state.self.p2p_port}`,
    n === 0 ? "scanning for devices…" : `${n} device${n === 1 ? "" : "s"} nearby`,
  ];
  if (established > 0) bits.push(`${established} connected`);
  if (shares > 0) bits.push(`sharing ${shares} folder${shares === 1 ? "" : "s"}`);
  bits.push(`protocol: ${state.access.mode}`);
  $("homeSub").textContent = bits.join("  ·  ");
}

/* ================================================================== theme */

const THEME_KEY = "single-theme";
const mq = matchMedia("(prefers-color-scheme: dark)");

function applyTheme(mode) {
  const dark = mode === "dark" || (mode === "system" && mq.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.querySelector('meta[name="theme-color"]').setAttribute("content", dark ? "#0b0d10" : "#eef1f6");
}

function setTheme(mode, persist = true) {
  if (persist) localStorage.setItem(THEME_KEY, mode);
  applyTheme(mode);
  document.querySelectorAll("#themeSeg .chip").forEach((c) => c.classList.toggle("active", c.dataset.t === mode));
}

mq.addEventListener("change", () => {
  if ((localStorage.getItem(THEME_KEY) || "system") === "system") applyTheme("system");
});

$("themeSeg").addEventListener("click", (e) => {
  const chip = e.target.closest(".chip");
  if (chip) setTheme(chip.dataset.t);
});

/* ================================================================== ws */

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}/ws`);
  state.ws = ws;

  ws.onopen = () => {
    setWsStatus("on", "live");
    $("reconnectBanner").classList.add("hidden");
    state.reconnectSec = 1;
    /* re-apply the persisted Protocol choice — the server keeps it in memory
       only, so every fresh dashboard session re-teaches it */
    sendAccess();
  };
  ws.onclose = () => {
    setWsStatus("off", "offline");
    $("reconnectBanner").classList.remove("hidden");
    const delay = Math.min(state.reconnectSec, 8) * 1000;
    state.reconnectSec = Math.min(state.reconnectSec * 1.7, 8);
    setTimeout(connect, delay);
  };
  ws.onerror = () => ws.close();
  ws.onmessage = (e) => {
    let msg;
    try { msg = JSON.parse(e.data); } catch { return; }
    handle(msg);
  };
}

function send(obj) {
  if (state.ws && state.ws.readyState === 1) state.ws.send(JSON.stringify(obj));
  return state.ws && state.ws.readyState === 1;
}

function setWsStatus(cls, text) {
  $("wsLed").className = "led " + cls;
  $("wsText").textContent = text;
}

/* ================================================================== dispatch */

function handle(msg) {
  switch (msg.type) {
    case "hello":
      state.self = msg.self_identity;
      state.serviceType = msg.service_type;
      renderSelf();
      break;
    case "devices":
      state.devices = new Map(msg.devices.map((d) => [d.id, d]));
      renderDevices();
      break;
    case "session":
      upsertSession(msg.session);
      break;
    case "chat": {
      const s = state.sessions.get(msg.peer_id);
      if (!s) break;
      if (!s.dto.chat) s.dto.chat = [];
      const last = s.dto.chat[s.dto.chat.length - 1];
      const dup = last && last.dir === msg.line.dir && last.text === msg.line.text && last.ts === msg.line.ts;
      if (!dup) s.dto.chat.push(msg.line);
      if (state.activePeer === msg.peer_id) {
        const chatEl = $("chatMain").querySelector(".chat-view .chat");
        const hint = chatEl && chatEl.querySelector(".bubble.hint");
        if (hint) hint.remove();
        appendBubble(s, msg.line);
        s._rendered = (s.dto.chat || []).length;
      }
      else s.unread++;
      updateNavBadge();
      renderChatList();
      break;
    }
    case "file":
      handleFileEvent(msg.peer_id, msg.event);
      break;
    case "log":
      appendLog(msg.level, msg.msg, msg.ts);
      break;
    case "access-state":
      applyAccess(msg);
      break;
    case "bridge":
      state.bridge = msg.bridge;
      renderFiles();
      break;
    case "connect-request":
      state.reqQueue.push(msg);
      showNextRequest();
      break;
    case "pong":
      $("wsRtt").textContent = (Date.now() - state.wsPingSent) + " ms";
      break;
  }
}

/* ================================================================== self */

function renderSelf() {
  if (!state.self) return;
  const p = state.self.platform;

  /* devices page — identity card */
  $("selfIcon").innerHTML = ICONS[p] || ICONS.unknown;
  $("selfName").textContent = state.self.name;
  $("selfPlatform").textContent = LABEL[p] || p;
  $("selfPlatform").className = "platform-badge " + p;
  $("selfAddr").textContent = `${state.self.ip}:${state.self.p2p_port}`;
  $("selfHttp").textContent = location.host;
  $("svcType").textContent = state.serviceType;

  /* settings page */
  $("setName").textContent = state.self.name;
  $("setPlatform").textContent = LABEL[p] || p;
  $("setPlatform").className = "platform-badge " + p;
  $("setIp").textContent = state.self.ip;
  $("setP2p").textContent = String(state.self.p2p_port);
  $("setHttp").textContent = location.host;
  $("setSvc").textContent = state.serviceType;

  /* home page */
  updateHome();
}

/* rename — inline editor inside Settings › Identity */
$("renameBtn").addEventListener("click", () => {
  const row = $("nameRow");
  if (row.querySelector(".rename-row")) return;
  const wrap = document.createElement("div");
  wrap.className = "rename-row";
  const input = document.createElement("input");
  input.value = state.self ? state.self.name : "";
  input.maxLength = 48;
  input.placeholder = "device name…";
  const ok = document.createElement("button");
  ok.className = "btn small";
  ok.textContent = "Save";
  const cancel = document.createElement("button");
  cancel.className = "btn small ghost";
  cancel.textContent = "Cancel";
  ok.onclick = () => {
    const v = input.value.trim();
    if (v) send({ type: "rename", name: v });
    restore();
  };
  cancel.onclick = restore;
  input.onkeydown = (e) => { if (e.key === "Enter") ok.click(); if (e.key === "Escape") restore(); };
  function restore() {
    $("setName").classList.remove("hidden");
    $("renameBtn").classList.remove("hidden");
    wrap.remove();
  }
  $("setName").classList.add("hidden");
  $("renameBtn").classList.add("hidden");
  wrap.append(input, ok, cancel);
  row.appendChild(wrap);
  input.focus();
  input.select();
});

/* ================================================================== devices */

function renderDevices() {
  const grid = $("deviceGrid");
  const all = [...state.devices.values()];
  const list = all
    .filter((d) => {
      if (state.filter === "all") return true;
      if (state.filter === "other") return d.platform !== "android" && d.platform !== "windows";
      return d.platform === state.filter;
    })
    .sort((a, b) => a.name.localeCompare(b.name));

  $("emptyState").style.display = state.devices.size === 0 ? "" : "none";
  $("statDevices").textContent = state.devices.size;
  $("devCount").textContent = state.devices.size === 0
    ? ""
    : `${state.devices.size} device${state.devices.size === 1 ? "" : "s"} on this network`;
  updateHome();

  /* Skip the DOM rebuild when nothing relevant changed — keeps buttons
     stable for clicks/hovers across the periodic mDNS refresh cycle. */
  const sig = list.map((d) => {
    const s = state.sessions.get(d.id);
    return `${d.id}|${d.name}|${d.platform}|${d.ip}:${d.port}|${d.last_seen ? 1 : 0}|${s ? s.dto.state : ""}`;
  }).join(";") + `#f=${state.filter}`;
  if (sig === renderDevices._sig) return;
  renderDevices._sig = sig;

  grid.innerHTML = list
    .map((d) => {
      const s = state.sessions.get(d.id);
      let btn;
      if (s && s.dto.state === "established")
        btn = `<button class="btn ghost" data-act="open" data-id="${esc(d.id)}">Open chat</button>`;
      else if (s && s.dto.state === "connecting")
        btn = `<button class="btn ghost" disabled>Handshaking…</button>`;
      else if (s)
        btn = `<button class="btn" data-act="connect" data-id="${esc(d.id)}">Reconnect</button>`;
      else
        btn = `<button class="btn" data-act="connect" data-id="${esc(d.id)}">Connect</button>`;
      return `
      <div class="device-card" data-card="${esc(d.id)}">
        <div class="dev-top">
          <span class="dev-icon">${ICONS[d.platform] || ICONS.unknown}</span>
          <div>
            <div class="dev-name" title="${esc(d.name)}">${esc(d.name)}</div>
            <div class="dev-host dim mono">${esc(d.hostname)}</div>
          </div>
          <span class="platform-badge ${esc(d.platform)}">${LABEL[d.platform] || esc(d.platform)}</span>
        </div>
        <div class="dev-addr mono">${esc(d.ip)}:${d.port}</div>
        <div class="dev-foot">
          <span class="dim seen" data-seen="${d.last_seen}">just now</span>
          ${btn}
        </div>
      </div>`;
    })
    .join("");
}

$("deviceGrid").addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-act]");
  if (!btn) return;
  const id = btn.dataset.id;
  if (btn.dataset.act === "connect") {
    state.pendingConnect = id;
    send({ type: "connect", device_id: id });
    btn.disabled = true;
    btn.textContent = "Connecting…";
  } else {
    showPage("chats");
    openChat(id);
  }
});

$("filters").addEventListener("click", (e) => {
  const chip = e.target.closest(".chip");
  if (!chip) return;
  state.filter = chip.dataset.f;
  document.querySelectorAll("#filters .chip").forEach((c) => c.classList.toggle("active", c === chip));
  renderDevices();
});

$("refreshBtn").addEventListener("click", () => send({ type: "refresh" }));

/* ================================================================== chats */

function stateLabel(s) {
  if (s.direction === "in" && s.state === "connecting") return "HANDSHAKE ←";
  return { connecting: "SYN-SENT", established: "ESTABLISHED", closed: "CLOSED", error: "ERROR" }[s.state] || s.state;
}

function upsertSession(dto) {
  let s = state.sessions.get(dto.peer_id);
  const fresh = !s;
  if (s) {
    s.dto = Object.assign({}, s.dto, dto);
  } else {
    s = { dto, unread: 0, _rendered: 0 };
    state.sessions.set(dto.peer_id, s);
  }

  renderChatList();
  renderDevices();
  $("statSessions").textContent = [...state.sessions.values()].filter(
    (x) => x.dto.state === "established").length;

  /* Protected-mode peer: it answered auth-required — ask for the password */
  if (dto.state === "error" && dto.error && dto.error.indexOf("auth-required") === 0) {
    askPassword(dto.peer_id);
  }

  if (fresh) {
    /* navigate to the conversation only when it is the one this user just
       clicked Connect for — sessions replayed on page load (initial sync)
       or inbound connections never yank the user around */
    if (state.pendingConnect === dto.peer_id) {
      state.pendingConnect = null;
      showPage("chats");
      openChat(dto.peer_id);
    }
  } else if (state.activePeer === dto.peer_id) {
    updateChatView(s);
  }
}

function lastLinePreview(s) {
  const c = s.dto.chat || [];
  if (!c.length) return stateLabel(s.dto).toLowerCase();
  const l = c[c.length - 1];
  const who = l.dir === "out" ? "you: " : l.dir === "in" ? "" : "";
  return who + l.text;
}

function renderChatList() {
  const listEl = $("chatList");
  const items = [...state.sessions.values()].sort((a, b) => {
    const la = (a.dto.chat || []).length, lb = (b.dto.chat || []).length;
    return lb - la;
  });
  $("chatsEmpty").style.display = items.length === 0 ? "" : "none";
  listEl.querySelectorAll(".chat-item").forEach((el) => el.remove());
  for (const s of items) {
    const d = s.dto;
    const btn = document.createElement("button");
    btn.className = "chat-item" + (state.activePeer === d.peer_id ? " active" : "");
    btn.dataset.peer = d.peer_id;
    btn.innerHTML = `
      <span class="dev-icon">${ICONS[d.peer_platform] || ICONS.unknown}</span>
      <span class="grow">
        <span class="ci-name">${esc(d.peer_name)}</span>
        <span class="ci-sub">${esc(lastLinePreview(s).slice(0, 42))}</span>
      </span>
      ${s.unread ? `<span class="ci-unread">${s.unread}</span>` : ""}`;
    btn.onclick = () => openChat(d.peer_id);
    listEl.appendChild(btn);
  }
}

$("chatList").addEventListener("click", (e) => {
  const btn = e.target.closest(".chat-item");
  if (btn) openChat(btn.dataset.peer);
});

function openChat(peerId) {
  const s = state.sessions.get(peerId);
  if (!s) return;
  state.activePeer = peerId;
  s.unread = 0;
  updateNavBadge();

  const d = s.dto;
  $("chatPlaceholder").classList.add("hidden");
  const main = $("chatMain");
  const old = main.querySelector(".chat-view");
  if (old) old.remove();

  const view = document.createElement("div");
  view.className = "chat-view";
  view.innerHTML = `
    <header class="chat-head">
      <span class="dev-icon">${ICONS[d.peer_platform] || ICONS.unknown}</span>
      <div class="grow">
        <div class="chat-title">${esc(d.peer_name)}</div>
        <div class="chat-sub mono">${d.direction === "in" ? "←" : "→"} ${esc(d.peer_addr)}</div>
      </div>
      <span class="state-chip ${esc(d.state)}"></span>
      <span class="enc-badge hidden" data-enc></span>
      <span class="rtt-badge hidden"></span>
      <button class="hs-toggle" data-act="hs" title="Show / hide the TCP handshake timeline">handshake</button>
      <button class="icon-btn" title="Send BYE and close" data-act="close">✕</button>
    </header>
    <div class="hs-timeline"></div>
    <div class="chat"></div>
    <footer class="chat-input">
      <input type="text" placeholder="Message ${esc(d.peer_name)}…" maxlength="2000">
      <button class="icon-btn" data-act="file" title="Send a file" style="color:var(--accent)">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" width="16" height="16"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>
      </button>
      <button class="btn" data-act="send">Send</button>
    </footer>`;

  const input = view.querySelector("input");
  view.querySelector('[data-act="close"]').onclick = () =>
    send({ type: "disconnect", session_id: d.peer_id });
  view.querySelector('[data-act="send"]').onclick = () => submitChat(s, input);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submitChat(s, input);
  });
  view.querySelector('[data-act="file"]').onclick = () => pickFile(s);
  view.querySelector('[data-act="hs"]').onclick = (e) => {
    s.hsUser = true;
    s.showHs = !s.showHs;
    updateHsToggle(s, view);
  };

  main.appendChild(view);
  renderChatList();
  updateChatView(s, true);
}

function submitChat(s, input) {
  const text = input.value.trim();
  if (!text) return;
  if (send({ type: "chat", device_id: s.dto.peer_id, text })) input.value = "";
}

function updateChatView(s, fullRebuild = false) {
  const view = $("chatMain").querySelector(".chat-view");
  if (!view) return;
  const d = s.dto;

  const chip = view.querySelector(".state-chip");
  chip.textContent = stateLabel(d);
  chip.className = "state-chip " + d.state;
  if (d.error) chip.title = d.error;

  const rtt = view.querySelector(".rtt-badge");
  if (d.rtt_ms != null) {
    rtt.classList.remove("hidden");
    rtt.textContent = `RTT ${d.rtt_ms} ms`;
  }

  /* E2EE chip — green lock when the session runs on sealed frames */
  const enc = view.querySelector("[data-enc]");
  if (d.encrypted) {
    enc.classList.remove("hidden", "plain");
    enc.innerHTML = `${LOCK_IC}E2EE`;
    enc.title = "Sealed with AES-256-GCM — keys from the X25519 exchange during the handshake";
  } else if (d.state === "established") {
    enc.classList.remove("hidden");
    enc.classList.add("plain");
    enc.textContent = "no E2EE";
    enc.title = "This peer runs an older build — frames travel as plaintext";
  } else {
    enc.classList.add("hidden");
  }

  view.querySelector(".chat-input input").disabled = d.state !== "established";

  /* handshake disclosure: visible while connecting (interesting to watch),
     auto-collapsed once established unless the user pinned it open */
  if (!s.hsUser) s.showHs = d.state === "connecting";
  updateHsToggle(s, view);

  /* handshake timeline */
  const stepsHtml = (d.steps || [])
    .map((st) => {
      const cls =
        st.step === "established" ? "established" :
        st.step === "fail" ? "fail" : "done";
      const ms = st.ms != null ? ` · ${Number(st.ms).toFixed(1)} ms` : "";
      return `<span class="hs-step ${cls}" title="${esc(st.detail)}">${esc(st.label)}${ms}</span>`;
    })
    .join('<span class="hs-arrow">→</span>');
  const lastDetail = (d.steps || []).length ? d.steps[d.steps.length - 1].detail : "";
  view.querySelector(".hs-timeline").innerHTML =
    stepsHtml +
    (lastDetail ? `<div class="hs-detail">${esc(lastDetail)}</div>` : "") +
    (d.error ? `<div class="hs-detail" style="color:var(--err)">${esc(d.error)}</div>` : "");

  /* chat history — rebuild when the known length changed (open or echo) */
  const chatEl = view.querySelector(".chat");
  const chatLen = (d.chat || []).length;
  if (fullRebuild || chatLen !== s._rendered) {
    chatEl.innerHTML = "";
    if (chatLen === 0) {
      const hint = document.createElement("div");
      hint.className = "bubble sys hint";
      hint.textContent = "start of the conversation — say hi or attach a file";
      chatEl.appendChild(hint);
    }
    for (const line of d.chat || []) appendBubble(s, line, chatEl);
    s._rendered = chatLen;
  }
  renderTransfers(s, chatEl);
}

function updateHsToggle(s, view) {
  const tl = view.querySelector(".hs-timeline");
  const btn = view.querySelector('[data-act="hs"]');
  tl.classList.toggle("collapsed", !s.showHs);
  btn.classList.toggle("on", !!s.showHs);
  btn.textContent = s.showHs ? "handshake ⌃" : "handshake ⌄";
}

function appendBubble(s, line, chatEl) {
  const chat = chatEl || $("chatMain").querySelector(".chat-view .chat");
  if (!chat) return;
  const div = document.createElement("div");
  const time = new Date(line.ts).toLocaleTimeString();
  if (line.dir === "sys") {
    div.className = "bubble sys";
    div.textContent = line.text;
  } else {
    div.className = "bubble " + line.dir;
    div.textContent = line.text;
    const t = document.createElement("span");
    t.className = "btime";
    t.textContent = time;
    div.appendChild(t);
  }
  chat.appendChild(div);
  chat.scrollTop = chat.scrollHeight;
}

/* ================================================================== file events */

function renderTransfers(s, chatEl) {
  const chat = chatEl || $("chatMain").querySelector(".chat-view .chat");
  if (!chat) return;
  for (const t of s.dto.transfers || []) {
    let el = chat.querySelector(`[data-tid="${CSS.escape(t.tid)}"]`);
    if (!el) {
      el = document.createElement("div");
      el.className = "transfer";
      el.dataset.tid = t.tid;
      el.innerHTML = `
        <div class="t-head">
          <span class="t-name">${t.direction === "in" ? "↓" : "↑"} ${esc(t.name)}</span>
          <span class="t-status"></span>
        </div>
        <div class="bar"><div class="fill"></div></div>`;
      chat.appendChild(el);
      chat.scrollTop = chat.scrollHeight;
    }
    const pct = t.total > 0 ? Math.min(100, Math.round((t.bytes / t.total) * 100)) : 0;
    el.querySelector(".fill").style.width = pct + "%";
    el.querySelector(".bar").className = "bar " + (t.status === "done" ? "done" : t.status === "error" ? "error" : "");
    const statusEl = el.querySelector(".t-status");
    if (t.status === "in-progress") statusEl.textContent = fmtBytes(t.bytes) + " / " + fmtBytes(t.total);
    else if (t.status === "done") statusEl.textContent = "✓ " + (t.detail ? "→ " + t.detail.split("/").pop() : "complete");
    else statusEl.textContent = "✗ " + (t.detail || "failed");
  }
}

function handleFileEvent(peerId, ev) {
  const s = state.sessions.get(peerId);
  if (!s) return;
  if (!s.dto.chat) s.dto.chat = [];
  if (ev.status === "start") {
    s.dto.chat.push({ dir: "sys", text: `↓ ${ev.detail}`, ts: Date.now() });
  } else if (ev.status === "done" && ev.direction === "in" && ev.detail) {
    s.dto.chat.push({ dir: "sys", text: `✓ saved → ${ev.detail}`, ts: Date.now() });
  } else if (ev.status === "error") {
    s.dto.chat.push({ dir: "sys", text: `✗ ${ev.detail || "transfer failed"}`, ts: Date.now() });
  }
  if (state.activePeer === peerId) updateChatView(s);
  renderChatList();
}

/* ============================================================ file pickup */

let fileTargetSession = null;
$("filePicker").addEventListener("change", async (e) => {
  const file = e.target.files[0];
  e.target.value = "";
  if (!file || !fileTargetSession) return;
  await sendFile(fileTargetSession, file);
});

function pickFile(s) {
  fileTargetSession = s;
  $("filePicker").click();
}

async function sendFile(s, file) {
  const peerId = s.dto.peer_id;
  if (s.dto.state !== "established") return;
  const tid = "f" + Date.now().toString(36) + Math.random().toString(36).slice(2, 7);
  const buf = new Uint8Array(await file.arrayBuffer());

  let sha = "";
  if (window.crypto && crypto.subtle) {
    try {
      const digest = await crypto.subtle.digest("SHA-256", buf);
      sha = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
    } catch { /* insecure origin: skip integrity check */ }
  }

  send({ type: "file-begin", device_id: peerId, tid, name: file.name, size: file.size, sha256: sha });
  if (!s.dto.chat) s.dto.chat = [];
  s.dto.chat.push({ dir: "sys", text: `↑ sending “${file.name}” (${fmtBytes(file.size)})…`, ts: Date.now() });
  if (state.activePeer === peerId) {
    const chatEl = $("chatMain").querySelector(".chat-view .chat");
    appendBubble(s, s.dto.chat[s.dto.chat.length - 1], chatEl);
    s._rendered = s.dto.chat.length;
  }

  const CHUNK = 48 * 1024;
  for (let seq = 0, off = 0; off < buf.length; seq++, off += CHUNK) {
    const slice = buf.subarray(off, Math.min(off + CHUNK, buf.length));
    send({ type: "file-chunk", device_id: peerId, tid, seq, data: bufToB64(slice) });
    await sleep(4);   /* gentle pacing so the WS + TCP writer can keep up */
  }
  send({ type: "file-end", device_id: peerId, tid });
}

function bufToB64(bytes) {
  let bin = "";
  const CH = 0x8000;
  for (let i = 0; i < bytes.length; i += CH) {
    bin += String.fromCharCode.apply(null, bytes.subarray(i, i + CH));
  }
  return btoa(bin);
}

/* ================================================================== files page
   Sharing (set-shares over the WS) + browsing a peer's shares over the
   loopback-only REST bridge (/api/bridge/*). The E2EE badge mirrors the
   session's `encrypted` flag. */

const FOLDER_IC = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M3.5 7V5.5A1.5 1.5 0 0 1 5 4h4l2 2.5h8A1.5 1.5 0 0 1 20.5 8V9"/><path d="M3.5 7a1.5 1.5 0 0 0-1.5 1.5l1.8 8.6A1.5 1.5 0 0 0 5.3 18.5h13.9a1.5 1.5 0 0 0 1.5-1.2l1.8-8.6A1.5 1.5 0 0 0 21 7H3.5z"/></svg>`;
const FILE_IC = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round"><path d="M13.5 3.5H7A1.5 1.5 0 0 0 5.5 5v14A1.5 1.5 0 0 0 7 20.5h10a1.5 1.5 0 0 0 1.5-1.5V8.5L13.5 3.5z"/><path d="M13.5 3.5V8.5H18.5"/></svg>`;
const LOCK_IC = `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="5" y="11" width="14" height="9" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/></svg>`;

function fsLogPath(segs) {
  return "/" + segs.map((s) => encodeURIComponent(s)).join("/");
}

function renderFiles() {
  renderShares();
  renderPeers();
  renderBrowserHead();
  updateHome();
}

/* ---- sharing card ---- */

function renderShares() {
  const box = $("shareList");
  const shares = (state.bridge && state.bridge.shares) || [];
  if (!shares.length) {
    box.innerHTML = `<div class="share-empty">Nothing shared yet — add a folder below. Connected devices see it in their Files page.</div>`;
    return;
  }
  box.innerHTML = shares.map((s, i) => `
    <div class="share-row">
      <span class="sr-icon">${FOLDER_IC}</span>
      <div class="grow">
        <div class="sr-title">${esc(s.title)}</div>
        <div class="sr-path mono">${esc(s.path)}</div>
      </div>
      <button class="btn ghost" data-del="${i}" title="Stop sharing this folder">✕ Stop</button>
    </div>`).join("");
}

$("shareList").addEventListener("click", (e) => {
  const btn = e.target.closest("button[data-del]");
  if (!btn || !state.bridge) return;
  const shares = state.bridge.shares.filter((_, i) => i !== Number(btn.dataset.del))
    .map((s) => ({ title: s.title, path: s.path }));
  send({ type: "set-shares", shares }); // server echoes a fresh bridge snapshot
});

function addShare() {
  if (!state.bridge) return;
  const input = $("sharePath");
  const path = input.value.trim().replace(/[\\/]+$/, "");
  if (!path) { input.focus(); return; }
  const shares = state.bridge.shares.map((s) => ({ title: s.title, path: s.path }));
  shares.push({ title: "", path }); // server derives the title from the basename
  send({ type: "set-shares", shares });
  input.value = "";
  $("fsLoopback").classList.add("hidden");
  $("shareHint").textContent = "Adding — folders that do not exist are ignored (see Activity).";
  setTimeout(() => { $("shareHint").textContent = "The folder must already exist — its name becomes the title peers see."; }, 4000);
}

$("shareAdd").addEventListener("click", addShare);
$("sharePath").addEventListener("keydown", (e) => { if (e.key === "Enter") addShare(); });

/* ---- connected devices card ---- */

function renderPeers() {
  const box = $("bridgePeers");
  const b = state.bridge;
  const peers = (b && b.peers) || [];
  const hint = $("drivesHint");
  hint.textContent = b && b.drives_enabled
    ? "Each connected device maps its own drive letter in This PC — one more device, one more drive."
    : "Connect to a device from the Devices page, then browse its shares here. On Windows each device also maps a drive letter in This PC.";
  if (!peers.length) {
    box.innerHTML = `<div class="share-empty">No connected devices yet. Connect from the <b>Devices</b> page — its shares then appear here.</div>`;
    return;
  }
  box.innerHTML = peers.map((p) => {
    const on = p.state === "established";
    const enc = p.encrypted
      ? `<span class="enc-badge" title="Every frame on this channel is sealed with AES-256-GCM after the X25519 key exchange">${LOCK_IC}E2EE</span>`
      : `<span class="enc-badge plain" title="This peer runs an older build — frames travel as plaintext">no E2EE</span>`;
    const drive = p.drive ? `<span class="drive-chip" title="Mapped in This PC">${esc(p.drive)}</span>` : "";
    const plat = `<span class="platform-badge ${esc(p.peer_platform)}">${LABEL[p.peer_platform] || esc(p.peer_platform)}</span>`;
    return `
    <div class="peer-card ${on ? "clickable" : "off"}" data-peer="${esc(p.peer_id)}" title="${on ? "Browse this device's shares" : "Handshake not established"}">
      <span class="dev-icon">${ICONS[p.peer_platform] || ICONS.unknown}</span>
      <div class="grow">
        <div class="pc-title">${esc(p.peer_name)}</div>
        <div class="pc-sub">${on ? "tap to browse its shares" : "connecting…"}</div>
      </div>
      <div class="pc-badges">${drive}${enc}${plat}</div>
    </div>`;
  }).join("");
}

$("bridgePeers").addEventListener("click", (e) => {
  const card = e.target.closest(".peer-card.clickable");
  if (card) openFs(card.dataset.peer, []);
});

/* ---- peer browser ---- */

function renderBrowserHead() {
  const p = peerById(state.filesPeer);
  const browser = $("filesBrowser");
  if (!p) { browser.classList.add("hidden"); return; }
  browser.classList.remove("hidden");
  $("fsIcon").innerHTML = ICONS[p.peer_platform] || ICONS.unknown;
  $("fsTitle").textContent = p.peer_name;
  const enc = $("fsEnc");
  if (p.encrypted) { enc.classList.remove("hidden", "plain"); }
  else { enc.classList.remove("hidden"); enc.classList.add("plain"); enc.textContent = "no E2EE"; }
  if (p.encrypted) enc.innerHTML = `${LOCK_IC}E2EE`;

  /* breadcrumb: peer name › Shares › segment … */
  const crumbs = [`<span class="crumb" data-c="-1">Shares</span>`];
  state.filesPath.forEach((seg, i) => {
    crumbs.push(`<span class="sep">/</span><span class="crumb${i === state.filesPath.length - 1 ? " here" : ""}" data-c="${i}">${esc(seg)}</span>`);
  });
  $("fsCrumb").innerHTML = crumbs.join("");
}

function peerById(id) {
  const b = state.bridge;
  return b && b.peers.find((p) => p.peer_id === id) || null;
}

$("fsCrumb").addEventListener("click", (e) => {
  const c = e.target.closest(".crumb[data-c]");
  if (!c || c.classList.contains("here")) return;
  const n = Number(c.dataset.c);
  if (n < 0) state.filesPath = [];
  else state.filesPath = state.filesPath.slice(0, n + 1);
  fetchFs();
});

function openFs(peerId, path) {
  state.filesPeer = peerId;
  state.filesPath = path || [];
  $("fsLoopback").classList.add("hidden");
  fetchFs();
}

function fsStatus(text, cls) {
  const el = $("fsStatus");
  el.textContent = text || "";
  el.className = "fs-status " + (cls || "dim");
}

async function fetchFs() {
  if (!state.filesPeer) return;
  renderBrowserHead();
  const list = $("fsList");
  list.innerHTML = `<div class="fs-empty">loading…</div>`;
  const q = `peer=${encodeURIComponent(state.filesPeer)}&path=${encodeURIComponent(fsLogPath(state.filesPath))}`;
  try {
    const r = await fetch(`/api/bridge/list?${q}`);
    if (r.status === 403) {
      $("fsLoopback").classList.remove("hidden");
      list.innerHTML = `<div class="fs-empty">blocked</div>`;
      return;
    }
    const data = await r.json();
    if (!data.ok) throw new Error(data.error || "listing failed");
    /* a good listing clears a stale error — success notes (✓ saved) stay */
    if ($("fsStatus").classList.contains("err")) fsStatus("");
    if (!data.entries.length) {
      list.innerHTML = `<div class="fs-empty">empty folder</div>`;
      return;
    }
    list.innerHTML = data.entries.map((en) => {
      if (en.kind === "dir") {
        return `<div class="fs-row dir" data-open="${esc(en.name)}">
          <span class="f-ic">${FOLDER_IC}</span>
          <span class="f-name">${esc(en.name)}</span>
          <span class="f-size">—</span>
          <span class="f-mod">${new Date(en.modified).toLocaleString()}</span>
          <span class="f-act"></span>
        </div>`;
      }
      const q2 = `peer=${encodeURIComponent(state.filesPeer)}&path=${encodeURIComponent(fsLogPath(state.filesPath.concat(en.name)))}`;
      return `<div class="fs-row">
        <span class="f-ic">${FILE_IC}</span>
        <span class="f-name">${esc(en.name)}</span>
        <span class="f-size">${fmtBytes(en.size)}</span>
        <span class="f-mod">${new Date(en.modified).toLocaleString()}</span>
        <span class="f-act"><a class="btn small ghost" href="/api/bridge/file?${q2}" download="${esc(en.name)}">Download</a></span>
      </div>`;
    }).join("");
  } catch (err) {
    list.innerHTML = `<div class="fs-empty">—</div>`;
    fsStatus(err.message || "listing failed", "err");
  }
}

$("fsList").addEventListener("click", (e) => {
  const row = e.target.closest(".fs-row.dir");
  if (!row) return;
  state.filesPath.push(row.dataset.open);
  fetchFs();
});

$("fsRefresh").addEventListener("click", fetchFs);

/* new folder */
$("fsMkdirBtn").addEventListener("click", () => {
  $("fsMkdirRow").classList.remove("hidden");
  $("fsMkdirName").value = "";
  $("fsMkdirName").focus();
});
$("fsMkdirCancel").addEventListener("click", () => $("fsMkdirRow").classList.add("hidden"));

async function fsMkdir() {
  const name = $("fsMkdirName").value.trim();
  if (!name) { $("fsMkdirName").focus(); return; }
  const q = `peer=${encodeURIComponent(state.filesPeer)}&path=${encodeURIComponent(fsLogPath(state.filesPath.concat(name)))}`;
  try {
    const r = await fetch(`/api/bridge/mkdir?${q}`, { method: "POST" });
    const data = await r.json();
    if (r.status === 403) { $("fsLoopback").classList.remove("hidden"); return; }
    if (!data.ok) throw new Error(data.error || "mkdir failed");
    $("fsMkdirRow").classList.add("hidden");
    fetchFs();
  } catch (err) {
    fsStatus(err.message || "mkdir failed", "err");
  }
}
$("fsMkdirGo").addEventListener("click", fsMkdir);
$("fsMkdirName").addEventListener("keydown", (e) => { if (e.key === "Enter") fsMkdir(); });

/* upload — XHR for progress events */
$("fsUpload").addEventListener("click", () => { if (state.filesPeer) $("filesPicker").click(); });

$("filesPicker").addEventListener("change", (e) => {
  const file = e.target.files[0];
  e.target.value = "";
  if (!file || !state.filesPeer) return;
  const target = state.filesPath.concat(file.name);
  const q = `peer=${encodeURIComponent(state.filesPeer)}&path=${encodeURIComponent(fsLogPath(target))}`;
  const xhr = new XMLHttpRequest();
  xhr.open("PUT", `/api/bridge/upload?${q}`);
  fsStatus(`↑ ${file.name} — 0%`, "dim");
  xhr.upload.onprogress = (ev) => {
    if (ev.lengthComputable) {
      fsStatus(`↑ ${file.name} — ${Math.round((ev.loaded / ev.total) * 100)}%`, "dim");
    }
  };
  xhr.onload = () => {
    if (xhr.status === 403) { $("fsLoopback").classList.remove("hidden"); return; }
    try {
      const data = JSON.parse(xhr.responseText);
      if (data.ok) { fsStatus(`✓ saved “${file.name}” on the peer`, "ok"); fetchFs(); }
      else fsStatus(data.error || "upload failed", "err");
    } catch { fsStatus("upload failed (" + xhr.status + ")", "err"); }
  };
  xhr.onerror = () => fsStatus("upload failed — connection error", "err");
  xhr.send(file);
});

/* ================================================================== badge */

function updateNavBadge() {
  const total = [...state.sessions.values()].reduce((n, s) => n + (s.unread || 0), 0);
  const badge = $("navBadge");
  badge.textContent = total > 99 ? "99+" : total;
  badge.classList.toggle("hidden", total === 0);
}

/* ================================================================== connect requests
   Private mode: the server holds the handshake open until we decide. */

function showNextRequest() {
  const modal = $("reqModal");
  const r = state.reqQueue[0];
  if (!r) {
    modal.classList.add("hidden");
    return;
  }
  $("reqIcon").innerHTML = ICONS[r.platform] || ICONS.unknown;
  $("reqName").textContent = r.name;
  $("reqAddr").textContent = r.addr;
  $("reqPlatform").textContent = LABEL[r.platform] || r.platform;
  $("reqPlatform").className = "platform-badge " + r.platform;
  modal.classList.remove("hidden");
  /* mirror the server's 30 s approval window — expire locally too */
  clearTimeout(showNextRequest._timer);
  showNextRequest._timer = setTimeout(() => {
    if (state.reqQueue[0] === r) {
      state.reqQueue.shift();
      showNextRequest();
    }
  }, 31000);
}

function answerRequest(accept) {
  const r = state.reqQueue.shift();
  if (r) send({ type: "connect-reply", id: r.id, accept });
  showNextRequest();
}
$("reqAccept").addEventListener("click", () => answerRequest(true));
$("reqDecline").addEventListener("click", () => answerRequest(false));

/* ================================================================== protected peers
   The remote replied auth-required → collect the password and retry. */

function askPassword(peerId) {
  if (!peerId || state.passFor === peerId && !$("passModal").classList.contains("hidden")) return;
  const dev = state.devices.get(peerId);
  state.passFor = peerId;
  $("passPeer").textContent = dev
    ? `“${dev.name}” is protected — enter its connection password.`
    : "This device is protected — enter its connection password.";
  $("passInput").value = state.devicePasswords[peerId] || "";
  $("passModal").classList.remove("hidden");
  $("passInput").focus();
  $("passInput").select();
}

function submitPassword() {
  const id = state.passFor;
  const pw = $("passInput").value;
  $("passModal").classList.add("hidden");
  state.passFor = null;
  if (!id || !pw) return;
  state.devicePasswords[id] = pw;
  try { localStorage.setItem("single-device-pw", JSON.stringify(state.devicePasswords)); } catch {}
  state.pendingConnect = id;
  send({ type: "connect", device_id: id, password: pw });
}

$("passGo").addEventListener("click", submitPassword);
$("passCancel").addEventListener("click", () => {
  $("passModal").classList.add("hidden");
  state.passFor = null;
});
$("passInput").addEventListener("keydown", (e) => {
  if (e.key === "Enter") submitPassword();
  if (e.key === "Escape") $("passCancel").click();
});

/* ================================================================== log */

function appendLog(level, msg, ts) {
  state.logCount++;
  $("statLogs").textContent = state.logCount;
  if (state.paused) return;
  const box = $("console");
  const div = document.createElement("div");
  div.className = "log-line";
  div.dataset.lvl = level;
  const time = new Date(ts || Date.now()).toLocaleTimeString();
  div.innerHTML = `<span class="log-ts">${time}</span><span class="log-lvl ${esc(level)}">${esc(level)}</span><span class="log-msg">${esc(msg)}</span>`;
  if (state.logLvl !== "all" && level !== state.logLvl) div.classList.add("hidden");
  box.appendChild(div);
  while (box.children.length > 250) box.removeChild(box.firstChild);
  box.scrollTop = box.scrollHeight;
}

$("pauseLog").addEventListener("click", () => {
  state.paused = !state.paused;
  $("pauseLog").textContent = state.paused ? "Resume" : "Pause";
});
$("clearLog").addEventListener("click", () => {
  $("console").innerHTML = "";
  state.logCount = 0;
  $("statLogs").textContent = "0";
});

$("logFilters").addEventListener("click", (e) => {
  const chip = e.target.closest(".chip");
  if (!chip) return;
  state.logLvl = chip.dataset.lvl;
  document.querySelectorAll("#logFilters .chip").forEach((c) => c.classList.toggle("active", c === chip));
  $("console").querySelectorAll(".log-line").forEach((el) => {
    el.classList.toggle("hidden", state.logLvl !== "all" && el.dataset.lvl !== state.logLvl);
  });
});

/* ================================================================== tickers */

setInterval(() => {
  document.querySelectorAll("[data-seen]").forEach((el) => {
    const age = Math.max(0, Math.round((Date.now() - Number(el.dataset.seen)) / 1000));
    el.textContent = age < 3 ? "just now" : `seen ${age}s ago`;
  });
  const secs = Math.round(performance.now() / 1000);
  $("statUptime").textContent =
    secs < 60 ? secs + "s" : Math.floor(secs / 60) + "m " + (secs % 60) + "s";
}, 1000);

setInterval(() => {
  if (state.ws && state.ws.readyState === 1) {
    state.wsPingSent = Date.now();
    send({ type: "ping" });
  }
}, 15000);

/* ================================================================== misc */

function fmtBytes(n) {
  if (n < 1024) return n + " B";
  const u = ["KB", "MB", "GB"];
  let v = n, i = -1;
  do { v /= 1024; i++; } while (v >= 1024 && i < 2);
  return v.toFixed(1) + " " + u[i];
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* ================================================================== boot */

(function boot() {
  $("verTag").textContent = "v" + APP_VERSION;
  $("setVer").textContent = "v" + APP_VERSION;
  /* no choice saved yet → follow the OS (system) without persisting it */
  const savedTheme = localStorage.getItem(THEME_KEY);
  setTheme(savedTheme || "system", !!savedTheme);
  loadAccess();
  renderAccess();
  try { state.devicePasswords = JSON.parse(localStorage.getItem("single-device-pw") || "{}"); } catch {}
  applySide(localStorage.getItem(SIDE_KEY) === "mini");
  showPage("home");
  connect();
  appendLog("info", "dashboard booted — connecting to control channel…", Date.now());
})();
