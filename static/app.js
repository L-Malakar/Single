/* Single — dashboard client.
 *
 * Multi-page SPA: Home · Devices · Chats · Activity · Settings (see index.html).
 * Desktop keeps the collapsible sidebar; phones get an app bar on top and a
 * tab bar at the bottom. The chat area is WhatsApp-style: day dividers,
 * bubbles, file cards, wallpaper, sticky composer.
 *
 * The WebSocket protocol (see src/protocol.rs):
 *   ← hello {self_identity, service_type}
 *   ← devices {devices:[…]}          live mDNS registry
 *   ← session {session}              P2P session state (handshake steps, rtt…)
 *   ← chat {peer_id, line}           one chat line
 *   ← file {peer_id, event}          transfer progress
 *   ← log {level, msg}               server activity
 *   ← logs {lines}                   persisted activity tail (on connect)
 *   ← access-state {mode, has_password}   inbound policy (Home › Protocol)
 *   ← connect-request {id, name, …}       Private-mode approval prompt
 *   → ping / refresh / rename / connect {device_id, password?}
 *     disconnect / chat / file-* / set-access {mode, password}
 *     connect-reply {id, accept}
 *
 * Persistence: the server keeps name/protocol/chats/logs in single-state.json;
 * the browser remembers the last page, open conversation, drafts, theme and
 * sidebar state so a reload (or app restart) returns exactly where you left.
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
  logBuffer: [],            // lines held while the log is paused
  logLvl: "all",
  logCount: 0,
  reconnectSec: 1,
  wsPingSent: 0,
  pendingConnect: null,
  pendingOpenPeer: null,    // conversation to open once its session arrives
  serverBoot: 0,            // ms epoch of server start (from /api/health)
  /* Home › Protocol — mirrored from the server (authoritative) */
  access: { mode: "private", password: "" },
  reqQueue: [],             // pending Private-mode connect-requests
  passFor: null,            // peer awaiting a Protected-mode password
  devicePasswords: {},      // peer_id → last working password (prefill only)
};

/* ================================================================== toasts */

function toast(msg, kind = "info", ms = 3400) {
  const box = $("toasts");
  if (!box) return;
  const t = document.createElement("div");
  t.className = "toast " + kind;
  t.textContent = msg;
  box.appendChild(t);
  setTimeout(() => {
    t.classList.add("out");
    setTimeout(() => t.remove(), 350);
  }, ms);
}

/* ================================================================== splash */

const splashT0 = performance.now();

function hideSplash() {
  const el = $("splash");
  if (!el || el.classList.contains("out")) return;
  /* keep it up for a beat so fast reloads don't strobe */
  const wait = Math.max(0, 650 - (performance.now() - splashT0));
  setTimeout(() => {
    el.classList.add("out");
    setTimeout(() => el.remove(), 480);
  }, wait);
}

/* ================================================================== pages */

const PAGES = new Set(["home", "devices", "chats", "activity", "settings"]);
const PAGE_KEY = "single-page";
const CHAT_KEY = "single-chat";
const DRAFT_KEY = "single-drafts";

function showPage(name) {
  if (!PAGES.has(name)) name = "home";
  state.page = name;
  document.querySelectorAll(".page").forEach((p) => p.classList.toggle("active", p.id === `page-${name}`));
  document.querySelectorAll(".nav-item, .tab-item").forEach((b) =>
    b.classList.toggle("active", b.dataset.page === name));
  try { localStorage.setItem(PAGE_KEY, name); } catch {}
}

/* one handler for the desktop sidebar and the mobile tab bar */
document.addEventListener("click", (e) => {
  const btn = e.target.closest(".nav-item[data-page], .tab-item[data-page]");
  if (btn) showPage(btn.dataset.page);
});

/* ============================================================ sidebar collapse
   YouTube-style: the rail shrinks to icons only; the choice persists.
   (≤980 px the rail is always icon-only; ≤760 px it is replaced by the
   mobile app bar + tab bar — see style.css.) */

const SIDE_KEY = "single-side";

function applySide(mini) {
  document.body.classList.toggle("side-mini", !!mini);
  document.documentElement.classList.toggle("side-mini", !!mini);
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
$("sideLogoBtn").addEventListener("click", () => setSide(!document.body.classList.contains("side-mini")));
$("sideNameBtn").addEventListener("click", () => setSide(!document.body.classList.contains("side-mini")));

/* ================================================================== access */

const ACCESS_KEY = "single-access";

function loadAccess() {
  /* render cache only — the server is authoritative and pushes its state */
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
  toast("Protocol password saved");
}
$("protoPassSave").addEventListener("click", saveProtocolPassword);
$("protoPass").addEventListener("keydown", (e) => { if (e.key === "Enter") saveProtocolPassword(); });

/* Server pushes the authoritative policy (on connect and on every change). */
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
  const bits = [
    `${state.self.ip}:${state.self.p2p_port}`,
    n === 0 ? "scanning for devices…" : `${n} device${n === 1 ? "" : "s"} nearby`,
  ];
  if (established > 0) bits.push(`${established} connected`);
  bits.push(`protocol: ${state.access.mode}`);
  $("homeSub").textContent = bits.join("  ·  ");
}

/* ================================================================== theme */

const THEME_KEY = "single-theme";
const mq = matchMedia("(prefers-color-scheme: dark)");

function applyTheme(mode) {
  const dark = mode === "dark" || (mode === "system" && mq.matches);
  document.documentElement.dataset.theme = dark ? "dark" : "light";
  document.querySelectorAll('meta[name="theme-color"]').forEach((m) =>
    m.setAttribute("content", dark ? "#0b0d10" : "#eef1f6"));
}

function setTheme(mode, persist = true) {
  if (persist) { try { localStorage.setItem(THEME_KEY, mode); } catch {} }
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
    /* The Protocol choice is persisted server-side and pushed to us — no
       need to re-teach it on reconnect anymore. */
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
  if (state.ws && state.ws.readyState === 1) {
    state.ws.send(JSON.stringify(obj));
    return true;
  }
  return false;
}

function setWsStatus(cls, text) {
  const led = $("wsLed");
  if (led) led.className = "led " + cls;
  const t = $("wsText");
  if (t) t.textContent = text;
  /* mobile app bar: green dot = live */
  const dot = $("mLiveDot");
  const mtext = $("mLiveText");
  if (dot && mtext) {
    dot.className = "mtop-dot " + cls;
    mtext.textContent =
      cls === "on" ? "LIVE" : cls === "off" ? "OFFLINE" : "CONNECTING";
  }
}

/* ================================================================== dispatch */

function handle(msg) {
  switch (msg.type) {
    case "hello":
      state.self = msg.self_identity;
      state.serviceType = msg.service_type;
      renderSelf();
      fetchHealth();
      hideSplash();
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
        const hint = chatEl && chatEl.querySelector(".bubble.sys.hint");
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
    case "logs":
      /* persisted tail from the server — the Activity page picks up where
         the last session left off */
      for (const l of msg.lines || []) appendLog(l.level, l.msg, l.ts);
      break;
    case "access-state":
      applyAccess(msg);
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

async function fetchHealth() {
  try {
    const r = await fetch("/api/health");
    const j = await r.json();
    if (j && typeof j.uptime_secs === "number") {
      state.serverBoot = Date.now() - j.uptime_secs * 1000;
    }
  } catch { /* keep the page-uptime fallback */ }
}

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
      else
        btn = `<button class="btn" data-act="connect" data-id="${esc(d.id)}">${s ? "Reconnect" : "Connect"}</button>`;
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
  try { localStorage.setItem("single-filter", state.filter); } catch {}
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
    s = { dto, unread: 0, _rendered: 0, _lastDay: null, showHs: false, hsUser: false };
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

  if (state.pendingConnect === dto.peer_id) {
    state.pendingConnect = null;
    showPage("chats");
    openChat(dto.peer_id);
  }
  /* restore the conversation the user had open before a reload/restart */
  if (state.pendingOpenPeer === dto.peer_id) {
    state.pendingOpenPeer = null;
    showPage("chats");
    openChat(dto.peer_id);
  }
  if (!fresh && state.activePeer === dto.peer_id) {
    updateChatView(s);
  }
}

/* ---------- chat list (WhatsApp-style rows) ---------- */

function lastLineOf(s) {
  const c = s.dto.chat || [];
  return c.length ? c[c.length - 1] : null;
}

function chatStamp(s) {
  const l = lastLineOf(s);
  const ts = l ? l.ts : s.dto.started_ts;
  const d = new Date(ts);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleDateString([], { day: "numeric", month: "short" });
}

function lastLinePreview(s) {
  const l = lastLineOf(s);
  if (!l) return stateLabel(s.dto).toLowerCase();
  const who = l.dir === "out" ? "you: " : "";
  const text = l.text.length > 48 ? l.text.slice(0, 48) + "…" : l.text;
  return who + text;
}

function renderChatList() {
  const listEl = $("chatList");
  const items = [...state.sessions.values()].sort((a, b) => {
    const ta = (lastLineOf(a) || { ts: a.dto.started_ts }).ts;
    const tb = (lastLineOf(b) || { ts: b.dto.started_ts }).ts;
    return tb - ta; /* most recent first */
  });
  $("chatsEmpty").style.display = items.length === 0 ? "" : "none";
  listEl.querySelectorAll(".chat-item").forEach((el) => el.remove());
  for (const s of items) {
    const d = s.dto;
    const btn = document.createElement("button");
    btn.className = "chat-item" + (state.activePeer === d.peer_id ? " active" : "");
    btn.dataset.peer = d.peer_id;
    btn.innerHTML = `
      <span class="ci-avatar">${ICONS[d.peer_platform] || ICONS.unknown}</span>
      <span class="grow">
        <span class="ci-top">
          <span class="ci-name">${esc(d.peer_name)}</span>
          <span class="ci-time">${esc(chatStamp(s))}</span>
        </span>
        <span class="ci-sub">${esc(lastLinePreview(s))}</span>
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

/* ---------- drafts (kept per peer) ---------- */

function loadDrafts() {
  try { return JSON.parse(localStorage.getItem(DRAFT_KEY) || "{}"); } catch { return {}; }
}
function saveDraft(peerId, text) {
  const d = loadDrafts();
  if (text) d[peerId] = text.slice(0, 2000); else delete d[peerId];
  try { localStorage.setItem(DRAFT_KEY, JSON.stringify(d)); } catch {}
}

/* ---------- conversation view ---------- */

function openChat(peerId) {
  const s = state.sessions.get(peerId);
  if (!s) return;
  state.activePeer = peerId;
  s.unread = 0;
  s._lastDay = null;
  updateNavBadge();
  try { localStorage.setItem(CHAT_KEY, peerId); } catch {}
  document.body.classList.add("chat-open");

  const d = s.dto;
  $("chatPlaceholder").classList.add("hidden");
  const main = $("chatMain");
  const old = main.querySelector(".chat-view");
  if (old) old.remove();

  const view = document.createElement("div");
  view.className = "chat-view";
  view.innerHTML = `
    <header class="chat-head">
      <button class="back-btn" data-act="back" title="All chats" aria-label="Back to all chats">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M14.5 5.5 8 12l6.5 6.5"/></svg>
      </button>
      <span class="chat-avatar">${ICONS[d.peer_platform] || ICONS.unknown}</span>
      <div class="grow">
        <div class="chat-title">${esc(d.peer_name)}</div>
        <div class="chat-sub mono">${d.direction === "in" ? "←" : "→"} ${esc(d.peer_addr)}</div>
      </div>
      <span class="state-chip ${esc(d.state)}"></span>
      <span class="rtt-badge hidden"></span>
      <button class="hs-toggle" data-act="hs" title="Show / hide the TCP handshake timeline">handshake</button>
      <button class="icon-btn" title="Send BYE and close" data-act="close">✕</button>
    </header>
    <div class="hs-timeline"></div>
    <div class="chat-scroll">
      <div class="chat"></div>
      <button class="chat-fab hidden" data-act="fab" title="Jump to the latest message" aria-label="Jump to the latest message">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4.5v14"/><path d="m6 13 6 6 6-6"/></svg>
      </button>
    </div>
    <div class="chat-status hidden"></div>
    <footer class="chat-input">
      <button class="ci-attach" data-act="file" title="Send a file" aria-label="Send a file">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>
      </button>
      <textarea rows="1" placeholder="Message ${esc(d.peer_name)}…" maxlength="2000"></textarea>
      <button class="ci-send" data-act="send" title="Send" aria-label="Send message">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M21 3.5 10.5 14"/><path d="M21 3.5 14 21l-3.5-7L3 10.5 21 3.5z"/></svg>
      </button>
    </footer>`;

  const input = view.querySelector("textarea");
  input.value = loadDrafts()[peerId] || "";
  growTextarea(input);
  input.addEventListener("input", () => {
    growTextarea(input);
    saveDraft(peerId, input.value);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault(); /* Enter sends, Shift+Enter adds a newline */
      submitChat(s, input);
    }
  });

  const chatEl = view.querySelector(".chat");
  chatEl.addEventListener("scroll", () => updateFab(view));

  view.querySelector('[data-act="back"]').onclick = closeChatView;
  view.querySelector('[data-act="close"]').onclick = () => {
    send({ type: "disconnect", session_id: d.peer_id });
  };
  view.querySelector('[data-act="send"]').onclick = () => submitChat(s, input);
  view.querySelector('[data-act="file"]').onclick = () => pickFile(s);
  view.querySelector('[data-act="fab"]').onclick = () => {
    chatEl.scrollTop = chatEl.scrollHeight;
    updateFab(view);
  };
  view.querySelector('[data-act="hs"]').onclick = () => {
    s.hsUser = true;
    s.showHs = !s.showHs;
    updateHsToggle(s, view);
  };

  main.appendChild(view);
  renderChatList();
  updateChatView(s, true);
  if (window.matchMedia("(max-width: 760px)").matches) input.blur(); /* avoid popping the keyboard on open */
}

function closeChatView() {
  state.activePeer = null;
  try { localStorage.removeItem(CHAT_KEY); } catch {}
  document.body.classList.remove("chat-open");
  const view = $("chatMain").querySelector(".chat-view");
  if (view) view.remove();
  $("chatPlaceholder").classList.remove("hidden");
  renderChatList();
}

function growTextarea(el) {
  el.style.height = "auto";
  el.style.height = Math.min(el.scrollHeight, 120) + "px";
}

function submitChat(s, input) {
  const text = input.value.trim();
  if (!text) return;
  if (send({ type: "chat", device_id: s.dto.peer_id, text })) {
    input.value = "";
    growTextarea(input);
    saveDraft(s.dto.peer_id, "");
  } else {
    toast("Cannot send — the server connection is down", "err");
  }
}

/* ---------- date dividers + bubbles ---------- */

function dayKey(ts) {
  const d = new Date(ts);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
}

function dayLabel(ts) {
  const d = new Date(ts);
  const now = new Date();
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const that = new Date(d.getFullYear(), d.getMonth(), d.getDate());
  const diff = Math.round((today - that) / 86400000);
  if (diff === 0) return "TODAY";
  if (diff === 1) return "YESTERDAY";
  const sameYear = d.getFullYear() === now.getFullYear();
  return d.toLocaleDateString(undefined, {
    day: "numeric", month: "short",
    ...(sameYear ? {} : { year: "numeric" }),
  });
}

function nearBottom(chatEl) {
  return chatEl.scrollHeight - chatEl.scrollTop - chatEl.clientHeight < 140;
}

function updateFab(view) {
  const chatEl = view.querySelector(".chat");
  const fab = view.querySelector(".chat-fab");
  if (!chatEl || !fab) return;
  fab.classList.toggle("hidden", nearBottom(chatEl));
}

function appendBubble(s, line, chatEl, opts = {}) {
  const chat = chatEl || $("chatMain").querySelector(".chat-view .chat");
  if (!chat) return;
  const scroll = opts.scroll !== false;
  const wasNear = nearBottom(chat);

  /* day divider whenever the calendar day changes */
  const dk = dayKey(line.ts);
  if (s._lastDay !== dk) {
    const chip = document.createElement("div");
    chip.className = "day-chip";
    chip.textContent = dayLabel(line.ts);
    chat.appendChild(chip);
    s._lastDay = dk;
  }

  const div = document.createElement("div");
  const time = new Date(line.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (line.dir === "sys") {
    div.className = "bubble sys";
    div.textContent = line.text;
  } else {
    div.className = "bubble " + line.dir;
    const body = document.createElement("span");
    body.className = "btext";
    body.textContent = line.text;
    const meta = document.createElement("span");
    meta.className = "bmeta";
    meta.textContent = time;
    div.append(body, meta);
  }
  chat.appendChild(div);
  if (scroll && (wasNear || line.dir === "out" || line.dir === "sys")) {
    chat.scrollTop = chat.scrollHeight;
    const fab = chat.closest(".chat-scroll");
    if (fab) fab.querySelector(".chat-fab").classList.add("hidden");
  } else if (scroll) {
    const wrap = chat.closest(".chat-scroll");
    if (wrap) wrap.querySelector(".chat-fab").classList.remove("hidden");
  }
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
  } else {
    rtt.classList.add("hidden");
  }

  const input = view.querySelector(".chat-input textarea");
  const attach = view.querySelector(".ci-attach");
  const sendBtn = view.querySelector(".ci-send");
  const live = d.state === "established";
  input.disabled = !live;
  attach.disabled = !live;
  sendBtn.disabled = !live;
  input.placeholder = live
    ? `Message ${d.peer_name}…`
    : d.state === "connecting" ? "handshaking…" : "reconnect to continue the conversation";

  /* status strip: handshake progress / reconnect affordance */
  const strip = view.querySelector(".chat-status");
  if (d.state === "established") {
    strip.classList.add("hidden");
    strip.innerHTML = "";
  } else {
    strip.classList.remove("hidden");
    if (d.state === "connecting") {
      strip.innerHTML = `<span class="spin"></span> completing the TCP handshake…`;
    } else {
      const known = state.devices.has(d.peer_id);
      const why = d.state === "error" && d.error ? esc(d.error) : "connection closed";
      strip.innerHTML = `
        <span class="dim">${why}</span>
        <button class="btn small" data-act="reconnect" ${known ? "" : 'disabled title="Device is not on this network right now"'}>
          ${known ? "Reconnect" : "Retry when back"}
        </button>`;
      const btn = strip.querySelector('[data-act="reconnect"]');
      if (btn && known) {
        btn.onclick = () => {
          state.pendingConnect = d.peer_id;
          if (!send({ type: "connect", device_id: d.peer_id })) {
            toast("Cannot connect — the server connection is down", "err");
          }
        };
      }
    }
  }

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
    s._lastDay = null;
    if (chatLen === 0) {
      const hint = document.createElement("div");
      hint.className = "bubble sys hint";
      hint.textContent = "start of the conversation — say hi or attach a file";
      chatEl.appendChild(hint);
    }
    for (const line of d.chat || []) appendBubble(s, line, chatEl, { scroll: false });
    s._rendered = chatLen;
    chatEl.scrollTop = chatEl.scrollHeight;
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

/* ================================================================== file events */

function renderTransfers(s, chatEl) {
  const chat = chatEl || $("chatMain").querySelector(".chat-view .chat");
  if (!chat) return;
  for (const t of s.dto.transfers || []) {
    let el = chat.querySelector(`[data-tid="${CSS.escape(t.tid)}"]`);
    if (!el) {
      el = document.createElement("div");
      el.className = "transfer " + t.direction;
      el.dataset.tid = t.tid;
      el.innerHTML = `
        <div class="t-head">
          <span class="t-ic">${t.direction === "in"
            ? `<svg viewBox="0 0 24 24" ${GLYPH}><path d="M12 4v12"/><path d="m6.5 10.5 5.5 5.5 5.5-5.5"/><path d="M5 19.5h14"/></svg>`
            : `<svg viewBox="0 0 24 24" ${GLYPH}><path d="M12 19.5v-12"/><path d="m6.5 13 5.5-5.5 5.5 5.5"/><path d="M5 4.5h14"/></svg>`}</span>
          <span class="t-name">${esc(t.name)}</span>
          <span class="t-status"></span>
        </div>
        <div class="bar"><div class="fill"></div></div>`;
      chat.appendChild(el);
      const wrap = chat.closest(".chat-scroll");
      if (nearBottom(chat)) {
        chat.scrollTop = chat.scrollHeight;
        if (wrap) wrap.querySelector(".chat-fab").classList.add("hidden");
      }
    }
    const pct = t.total > 0 ? Math.min(100, Math.round((t.bytes / t.total) * 100)) : 0;
    el.querySelector(".fill").style.width = pct + "%";
    el.querySelector(".bar").className = "bar " + (t.status === "done" ? "done" : t.status === "error" ? "error" : "");
    const statusEl = el.querySelector(".t-status");
    if (t.status === "in-progress") statusEl.textContent = fmtBytes(t.bytes) + " / " + fmtBytes(t.total);
    else if (t.status === "done") statusEl.textContent = "✓ " + (t.detail ? "→ " + t.detail.split(/[\\/]/).pop() : "complete");
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

const MAX_SEND_BYTES = 256 * 1024 * 1024; /* mirrors the server's cap */
let fileTargetSession = null;
$("filePicker").addEventListener("change", async (e) => {
  const file = e.target.files[0];
  e.target.value = "";
  if (!file || !fileTargetSession) return;
  await sendFile(fileTargetSession, file);
});

function pickFile(s) {
  if (s.dto.state !== "established") {
    toast("Wait for the handshake to finish first", "warn");
    return;
  }
  fileTargetSession = s;
  $("filePicker").click();
}

async function sendFile(s, file) {
  const peerId = s.dto.peer_id;
  if (s.dto.state !== "established") return;
  if (file.size > MAX_SEND_BYTES) {
    toast(`“${file.name}” is ${fmtBytes(file.size)} — the transfer limit is 256 MB`, "err", 5000);
    return;
  }
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
    /* stop if the session died mid-transfer (the card turns into an error) */
    if (s.dto.state !== "established") {
      toast(`Transfer of “${file.name}” aborted — connection closed`, "err");
      return;
    }
    const slice = buf.subarray(off, Math.min(off + CHUNK, buf.length));
    if (!send({ type: "file-chunk", device_id: peerId, tid, seq, data: bufToB64(slice) })) {
      toast(`Transfer of “${file.name}” aborted — server connection lost`, "err");
      return;
    }
    /* backpressure: wait for the socket buffer to drain so a big file cannot
       pile up unbounded memory in the browser */
    let guard = 0;
    while (state.ws && state.ws.readyState === 1 && state.ws.bufferedAmount > 1024 * 1024) {
      await sleep(15);
      if (++guard > 4000) break; /* ~60 s stall → give up waiting */
    }
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

/* ================================================================== badge */

function updateNavBadge() {
  const total = [...state.sessions.values()].reduce((n, s) => n + (s.unread || 0), 0);
  for (const id of ["navBadge", "tabBadge"]) {
    const badge = $(id);
    if (!badge) continue;
    badge.textContent = total > 99 ? "99+" : total;
    badge.classList.toggle("hidden", total === 0);
  }
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
  if (state.paused) {
    /* keep buffering — Pause freezes the view, it does not discard */
    state.logBuffer.push({ level, msg, ts });
    if (state.logBuffer.length > 250) state.logBuffer.shift();
    return;
  }
  appendLogLine(level, msg, ts);
}

function appendLogLine(level, msg, ts) {
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
  if (!state.paused) {
    /* flush everything that happened while paused */
    for (const l of state.logBuffer) appendLogLine(l.level, l.msg, l.ts);
    state.logBuffer = [];
  }
});
$("clearLog").addEventListener("click", () => {
  $("console").innerHTML = "";
  state.logBuffer = [];
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
  /* uptime tracks the *server*, not this browser tab */
  const secs = state.serverBoot
    ? Math.max(0, Math.round((Date.now() - state.serverBoot) / 1000))
    : Math.round(performance.now() / 1000);
  $("statUptime").textContent =
    secs < 60 ? secs + "s"
    : secs < 3600 ? Math.floor(secs / 60) + "m " + (secs % 60) + "s"
    : Math.floor(secs / 3600) + "h " + Math.floor((secs % 3600) / 60) + "m";
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
  $("splashVer").textContent = "v" + APP_VERSION;

  /* no choice saved yet → follow the OS (system) without persisting it */
  const savedTheme = localStorage.getItem(THEME_KEY);
  setTheme(savedTheme || "system", !!savedTheme);
  loadAccess();
  renderAccess();
  try { state.devicePasswords = JSON.parse(localStorage.getItem("single-device-pw") || "{}"); } catch {}
  applySide(localStorage.getItem(SIDE_KEY) === "mini");

  /* restore where the user left off: page, open conversation, filter */
  const savedFilter = localStorage.getItem("single-filter");
  if (savedFilter) {
    state.filter = savedFilter;
    document.querySelectorAll("#filters .chip").forEach((c) =>
      c.classList.toggle("active", c.dataset.f === savedFilter));
  }
  showPage(localStorage.getItem(PAGE_KEY) || "home");
  /* re-open the conversation the user had open — but only when they were
     actually on the Chats page; the saved page always wins otherwise */
  const savedChat = localStorage.getItem(CHAT_KEY);
  if (savedChat && state.page === "chats") {
    state.pendingOpenPeer = savedChat;
    /* if the session is already here (e.g. page reload) open it now */
    if (state.sessions.has(savedChat)) {
      const p = state.pendingOpenPeer;
      state.pendingOpenPeer = null;
      showPage("chats");
      openChat(p);
    }
  }

  connect();
  appendLog("info", "dashboard booted — connecting to control channel…", Date.now());

  /* never let a stuck splash cover the app */
  setTimeout(hideSplash, 5000);
})();
