/* p2p-bridge dashboard — vanilla JS WebSocket client.
 *
 * Protocol (see src/protocol.rs):
 *   ← hello {self_identity, service_type}
 *   ← devices {devices:[…]}          live mDNS registry
 *   ← session {session}              P2P session state (handshake steps, rtt…)
 *   ← chat {peer_id, line}           one chat line
 *   ← file {peer_id, event}          transfer progress
 *   ← log {level, msg}               server activity
 *   → ping / refresh / rename / connect / disconnect / chat / file-*
 */

"use strict";

const $ = (id) => document.getElementById(id);
const esc = (s) =>
  String(s).replace(/[&<>"']/g, (c) =>
    ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));

/* Monochrome line glyphs (SF-Symbols style) — inserted via innerHTML templates. */
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
  devices: new Map(),      // id → device dto
  sessions: new Map(),     // peer_id → { dto, el refs, chatRendered }
  filter: "all",
  paused: false,
  reconnectSec: 1,
  wsPingSent: 0,
  sessionSeq: 0,
};

/* ---------------------------------------------------------------- ws */

function connect() {
  const proto = location.protocol === "https:" ? "wss" : "ws";
  const ws = new WebSocket(`${proto}://${location.host}/ws`);
  state.ws = ws;

  ws.onopen = () => {
    setWsStatus("on", "live");
    $("reconnectBanner").classList.add("hidden");
    state.reconnectSec = 1;
  };
  ws.onclose = () => {
    setWsStatus("off", "disconnected");
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
}

function setWsStatus(cls, text) {
  $("wsLed").className = "led " + cls;
  $("wsText").textContent = text;
}

/* ---------------------------------------------------------- dispatch */

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
      if (s) {
        if (s.dto.chat) s.dto.chat.push(msg.line);
        appendBubble(s, msg.line);
      }
      break;
    }
    case "file":
      handleFileEvent(msg.peer_id, msg.event);
      break;
    case "log":
      appendLog(msg.level, msg.msg, msg.ts);
      break;
    case "pong":
      $("wsRtt").textContent = (Date.now() - state.wsPingSent) + " ms";
      break;
  }
}

/* -------------------------------------------------------------- self */

function renderSelf() {
  if (!state.self) return;
  const p = state.self.platform;
  $("selfPlatform").textContent = LABEL[p] || p;
  $("selfPlatform").className = "platform-badge " + p;
  $("selfName").textContent = state.self.name;
  $("selfAddr").textContent = `${state.self.ip}:${state.self.p2p_port}`;
  $("selfSvc").textContent = state.serviceType;
}

$("editNameBtn").addEventListener("click", () => {
  const card = $("selfCard");
  if (card.querySelector(".rename-row")) return;
  const row = document.createElement("div");
  row.className = "rename-row";
  row.style.marginTop = "6px";
  const input = document.createElement("input");
  input.value = state.self ? state.self.name : "";
  input.maxLength = 48;
  input.placeholder = "device name…";
  const ok = document.createElement("button");
  ok.className = "btn small";
  ok.textContent = "Save";
  const cancel = document.createElement("button");
  cancel.className = "btn small ghost";
  cancel.textContent = "✕";
  ok.onclick = () => {
    const v = input.value.trim();
    if (v) send({ type: "rename", name: v });
    row.remove();
  };
  cancel.onclick = () => row.remove();
  input.onkeydown = (e) => { if (e.key === "Enter") ok.click(); };
  row.append(input, ok, cancel);
  card.appendChild(row);
  input.focus();
});

/* ---------------------------------------------------------- devices */

function renderDevices() {
  const grid = $("deviceGrid");
  const list = [...state.devices.values()].filter((d) => {
    if (state.filter === "all") return true;
    if (state.filter === "other") return d.platform !== "android" && d.platform !== "windows";
    return d.platform === state.filter;
  });

  $("emptyState").style.display = state.devices.size === 0 ? "" : "none";
  $("statDevices").textContent = state.devices.size;
  $("statAndroid").textContent = [...state.devices.values()].filter((d) => d.platform === "android").length;
  $("statWindows").textContent = [...state.devices.values()].filter((d) => d.platform === "windows").length;

  grid.innerHTML = list
    .map((d) => {
      const hasSession = state.sessions.has(d.id);
      const btn = hasSession
        ? `<button class="btn ghost" data-act="open" data-id="${esc(d.id)}">Open</button>`
        : `<button class="btn" data-act="connect" data-id="${esc(d.id)}">Connect</button>`;
      return `
      <div class="device-card" data-card="${esc(d.id)}">
        <div class="dev-top">
          <span class="dev-icon">${ICONS[d.platform] || ICONS.unknown}</span>
          <div>
            <div class="dev-name">${esc(d.name)}</div>
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
    send({ type: "connect", device_id: id });
    btn.disabled = true;
    btn.textContent = "Connecting…";
    setTimeout(() => renderDevices(), 1200);
  } else {
    const s = state.sessions.get(id);
    if (s) s.el.scrollIntoView({ behavior: "smooth", block: "center" });
  }
});

$("filters").addEventListener("click", (e) => {
  const chip = e.target.closest(".chip");
  if (!chip) return;
  state.filter = chip.dataset.f;
  document.querySelectorAll("#filters .chip").forEach((c) => c.classList.toggle("active", c === chip));
  renderDevices();
});

/* --------------------------------------------------------- sessions */

function stateLabel(s) {
  if (s.direction === "in" && s.state === "connecting") return "HANDSHAKE ←";
  return { connecting: "SYN-SENT", established: "ESTABLISHED", closed: "CLOSED", error: "ERROR" }[s.state] || s.state;
}

function upsertSession(dto) {
  let s = state.sessions.get(dto.peer_id);
  if (s) {
    s.dto = Object.assign({}, s.dto, dto);
  } else {
    s = { dto, chatRendered: 0, els: {} };
    state.sessions.set(dto.peer_id, s);
    buildSessionCard(s);
  }
  if (dto.chat && dto.chat.length) {
    // full history (new tab or new session): rebuild chat box FIRST,
    // so updateSessionCard→renderTransfers re-attaches widgets after it
    // instead of having them wiped by this rebuild.
    s.chatRendered = 0;
    s.els.chat.innerHTML = "";
    for (const line of dto.chat) appendBubble(s, line);
  }
  updateSessionCard(s);
  renderDevices();
}

function buildSessionCard(s) {
  const d = s.dto;
  const card = document.createElement("div");
  card.className = "session-card";
  card.innerHTML = `
    <div class="sess-head">
      <span class="dev-icon">${ICONS[d.peer_platform] || ICONS.unknown}</span>
      <div class="grow">
        <div class="sess-name">${esc(d.peer_name)}</div>
        <div class="sess-addr dim mono">${d.direction === "in" ? "←" : "→"} ${esc(d.peer_addr)}</div>
      </div>
      <span class="state-chip ${esc(d.state)}"></span>
      <span class="rtt-badge hidden"></span>
      <button class="icon-btn" title="Send BYE and close" data-act="close">✕</button>
    </div>
    <div class="hs-timeline"></div>
    <div class="chat"></div>
    <div class="chat-input">
      <input type="text" placeholder="Message ${esc(d.peer_name)}…" maxlength="2000">
      <button class="btn" data-act="send">Send</button>
      <button class="btn ghost" data-act="file" title="Send a file"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg></button>
    </div>`;

  const chatEl = card.querySelector(".chat");
  const input = card.querySelector("input");

  card.querySelector('[data-act="close"]').onclick = () => {
    send({ type: "disconnect", session_id: d.peer_id });
  };
  card.querySelector('[data-act="send"]').onclick = () => submitChat(s, input);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") submitChat(s, input);
  });
  card.querySelector('[data-act="file"]').onclick = () => pickFile(s);

  s.els = {
    root: card,
    chip: card.querySelector(".state-chip"),
    rtt: card.querySelector(".rtt-badge"),
    timeline: card.querySelector(".hs-timeline"),
    chat: chatEl,
    input,
  };
  $("sessionList").appendChild(card);
}

function submitChat(s, input) {
  const text = input.value.trim();
  if (!text) return;
  if (send({ type: "chat", device_id: s.dto.peer_id, text })) input.value = "";
}

function updateSessionCard(s) {
  const d = s.dto;
  $("sessionsEmpty").style.display = state.sessions.size === 0 ? "" : "none";
  $("statSessions").textContent = [...state.sessions.values()].filter(
    (x) => x.dto.state === "established").length;

  s.els.chip.textContent = stateLabel(d);
  s.els.chip.className = "state-chip " + d.state;
  if (d.error) s.els.chip.title = d.error;

  if (d.rtt_ms != null) {
    s.els.rtt.classList.remove("hidden");
    s.els.rtt.textContent = `RTT ${d.rtt_ms} ms`;
  }
  s.els.input.disabled = d.state !== "established";

  // handshake timeline
  const stepsHtml = d.steps
    .map((st) => {
      const cls =
        st.step === "established" ? "established" :
        st.step === "fail" ? "fail" :
        st.ms != null || st.step === "hello-ack" ? "done" : "done";
      const ms = st.ms != null ? ` · ${Number(st.ms).toFixed(1)} ms` : "";
      return `<span class="hs-step ${cls}" title="${esc(st.detail)}">${esc(st.label)}${ms}</span>`;
    })
    .join('<span class="hs-arrow">→</span>');
  const lastDetail = d.steps.length ? d.steps[d.steps.length - 1].detail : "";
  s.els.timeline.innerHTML =
    stepsHtml +
    (lastDetail ? `<div class="hs-detail">${esc(lastDetail)}</div>` : "") +
    (d.error ? `<div class="hs-detail" style="color:var(--err)">${esc(d.error)}</div>` : "");

  renderTransfers(s);
}

function appendBubble(s, line) {
  const chat = s.els.chat;
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
  s.chatRendered++;
}

/* ------------------------------------------------------ file events */

function renderTransfers(s) {
  // Reconcile transfer widgets inside the chat box from session.transfers.
  for (const t of s.dto.transfers || []) {
    let el = s.els.chat.querySelector(`[data-tid="${CSS.escape(t.tid)}"]`);
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
      s.els.chat.appendChild(el);
      s.els.chat.scrollTop = s.els.chat.scrollHeight;
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
  if (ev.status === "start") {
    appendBubble(s, { dir: "sys", text: `↓ ${ev.detail}`, ts: Date.now() });
  } else if (ev.status === "done" || ev.status === "error") {
    if (ev.status === "done" && ev.direction === "in" && ev.detail) {
      appendBubble(s, { dir: "sys", text: `✓ saved → ${ev.detail}`, ts: Date.now() });
    }
  }
  // progress is rendered via session.transfers updates
}

/* ------------------------------------------------------ file pickup */

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
  appendBubble(s, { dir: "sys", text: `↑ sending “${file.name}” (${fmtBytes(file.size)})…`, ts: Date.now() });

  const CHUNK = 48 * 1024;
  for (let seq = 0, off = 0; off < buf.length; seq++, off += CHUNK) {
    const slice = buf.subarray(off, Math.min(off + CHUNK, buf.length));
    send({ type: "file-chunk", device_id: peerId, tid, seq, data: bufToB64(slice) });
    // gentle pacing so the WS + TCP writer can keep up
    await sleep(4);
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

/* ------------------------------------------------------------- log */

function appendLog(level, msg, ts) {
  if (state.paused) return;
  const box = $("console");
  const div = document.createElement("div");
  div.className = "log-line";
  const time = new Date(ts || Date.now()).toLocaleTimeString();
  div.innerHTML = `<span class="log-ts">${time}</span><span class="log-lvl ${esc(level)}">${esc(level)}</span><span class="log-msg">${esc(msg)}</span>`;
  box.appendChild(div);
  while (box.children.length > 250) box.removeChild(box.firstChild);
  box.scrollTop = box.scrollHeight;
}

$("pauseLog").addEventListener("click", () => {
  state.paused = !state.paused;
  $("pauseLog").textContent = state.paused ? "Resume" : "Pause";
});
$("clearLog").addEventListener("click", () => ($("console").innerHTML = ""));

/* ---------------------------------------------------------- tickers */

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

/* ------------------------------------------------------------- misc */

function fmtBytes(n) {
  if (n < 1024) return n + " B";
  const u = ["KB", "MB", "GB"];
  let v = n, i = -1;
  do { v /= 1024; i++; } while (v >= 1024 && i < 2);
  return v.toFixed(1) + " " + u[i];
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/* ------------------------------------------------------------- boot */

connect();
appendLog("info", "dashboard booted — connecting to control channel…", Date.now());
