/* Single — UI logic (vanilla JS, talks to the Rust core via Tauri IPC) */
"use strict";

/* ------------------------------------------------------------------ utils */

const $ = (sel) => document.querySelector(sel);
const $$ = (sel) => Array.from(document.querySelectorAll(sel));

function esc(s) {
  return String(s ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

function fmtSize(n) {
  n = Number(n) || 0;
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = n, u = 0;
  while (v >= 1024 && u < units.length - 1) { v /= 1024; u++; }
  return u === 0 ? `${n} B` : `${v.toFixed(1)} ${units[u]}`;
}

function fmtDate(epoch) {
  if (!epoch) return "";
  const d = new Date(Number(epoch) * 1000);
  return d.toLocaleDateString() + " " + d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function toast(message, isError) {
  const box = document.createElement("div");
  box.className = "toast" + (isError ? " error" : "");
  box.textContent = message;
  $("#toasts").appendChild(box);
  setTimeout(() => box.remove(), 5000);
}

/* ------------------------------------------------------------- tauri bridge */

const T = window.__TAURI__;
const invoke = (cmd, args = {}) => T.core.invoke(cmd, args);
const listen = (ev, cb) => T.event.listen(ev, cb);
const dialog = () => T.dialog;

/* ------------------------------------------------------------------- state */

const state = {
  info: null,
  devices: [],
  view: "devices",
  files: {
    peer: null,                 // null = this device
    paths: {},                  // peer id or "local" -> current path
    entries: [],
  },
  transferCount: 0,
};

function currentFilesPath() {
  const key = state.files.peer ?? "local";
  return state.files.paths[key] ?? "";
}

/* ------------------------------------------------------------------- modals */

function openModal(html) {
  $("#modal-box").innerHTML = html;
  $("#modal-root").classList.remove("hidden");
}
function closeModal() {
  $("#modal-root").classList.add("hidden");
  $("#modal-box").innerHTML = "";
}
$("#modal-backdrop").addEventListener("click", closeModal);

/* -------------------------------------------------------------------- views */

function switchView(name) {
  state.view = name;
  $$(".nav-item").forEach((b) => b.classList.toggle("active", b.dataset.view === name));
  $$(".view").forEach((v) => v.classList.toggle("active", v.id === "view-" + name));
  if (name === "devices") renderDevices();
  if (name === "files") { renderFileTabs(); refreshFiles(); }
  if (name === "transfers") renderTransfers();
  if (name === "settings") renderSettings();
  if (name === "about") renderAbout();
}

$$(".nav-item").forEach((b) => b.addEventListener("click", () => switchView(b.dataset.view)));

/* ------------------------------------------------------------------ devices */

async function refreshDevices() {
  try {
    state.devices = await invoke("list_devices");
    renderDevices();
  } catch (e) { toast(String(e), true); }
}

function renderDevices() {
  const grid = $("#device-grid");
  const others = state.devices.filter((d) => !d.is_self);
  if (others.length === 0) {
    grid.innerHTML = `<div class="empty">No other devices found yet.<br><br>
      Start Single on another phone or computer connected to the same network,
      or use “Add by IP”.</div>`;
    return;
  }
  grid.innerHTML = others.map((d) => {
    const online = d.paired ? d.online : true;
    const initial = esc((d.name || "?").trim().charAt(0).toUpperCase());
    const dot = `<span class="status-dot ${online ? "online" : "offline"}" title="${online ? "online" : "offline"}"></span>`;
    const pill = d.paired ? `<span class="pill paired">paired</span>` : `<span class="pill">new</span>`;
    const dl = d.drive_letter ? `<span class="pill">drive ${esc(d.drive_letter)}:</span>` : "";
    let actions = "";
    if (!d.paired) {
      actions = `<button class="btn small primary" data-act="pair" data-id="${esc(d.id)}" ${online ? "" : "disabled"}>Pair</button>`;
    } else {
      actions = `
        <button class="btn small" data-act="browse" data-id="${esc(d.id)}">Browse</button>
        ${state.info && state.info.platform === "windows"
          ? `<button class="btn small" data-act="map" data-id="${esc(d.id)}" ${online ? "" : "disabled"}>${d.drive_letter ? "Re-map drive" : "Map drive"}</button>`
          : ""}
        <button class="btn small danger" data-act="unpair" data-id="${esc(d.id)}">Unpair</button>`;
    }
    return `<div class="device-card">
      <div class="device-top">
        <div class="avatar">${initial}</div>
        <div>
          <div class="device-name">${esc(d.name)}</div>
          <div class="device-meta">${dot} ${esc(d.addr ?? "offline")} ${pill} ${dl}</div>
        </div>
      </div>
      <div class="device-actions">${actions}</div>
    </div>`;
  }).join("");

  $$("#device-grid .btn").forEach((b) => {
    b.addEventListener("click", () => deviceAction(b.dataset.act, b.dataset.id));
  });
}

async function deviceAction(act, id) {
  const dev = state.devices.find((d) => d.id === id);
  try {
    if (act === "browse") {
      state.files.peer = id;
      if (!(id in state.files.paths)) state.files.paths[id] = "";
      switchView("files");
    } else if (act === "pair") {
      await startPairFlow(id, dev?.name ?? "device");
    } else if (act === "unpair") {
      if (confirm(`Unpair from "${dev?.name}"?`)) {
        await invoke("unpair", { peerId: id });
        toast("Device removed");
        refreshDevices();
      }
    } else if (act === "map") {
      const letter = await invoke("win_map_drive", { peerId: id });
      toast(`Mapped as drive ${letter}:`);
      refreshDevices();
    }
  } catch (e) { toast(String(e), true); }
}

/* ------------------------------------------------------- pairing (initiator) */

let pairPollTimer = null;

async function startPairFlow(peerId, peerName) {
  let reqId;
  try {
    reqId = await invoke("pair_start", { peerId });
  } catch (e) {
    // "already paired on their side" completes instantly — just refresh
    toast(String(e), true);
    refreshDevices();
    return;
  }

  openModal(`
    <h2>Pairing with ${esc(peerName)}</h2>
    <p>Ask the other device to <b>allow</b> the request and tell you the
       6-digit PIN it shows.</p>
    <input class="pin-input" id="pair-pin" maxlength="6" inputmode="numeric"
           placeholder="••••••" autocomplete="off" />
    <p id="pair-status-line" class="hint">Waiting for approval…</p>
    <div class="row-end">
      <button class="btn ghost" id="pair-cancel">Cancel</button>
      <button class="btn primary" id="pair-submit">Enter PIN</button>
    </div>`);

  $("#pair-cancel").addEventListener("click", () => { stopPairPoll(); closeModal(); });
  $("#pair-submit").addEventListener("click", submitPairPin);
  $("#pair-pin").addEventListener("keydown", (ev) => { if (ev.key === "Enter") submitPairPin(); });
  $("#pair-pin").focus();

  let submitted = false;
  async function submitPairPin() {
    const pin = $("#pair-pin").value.trim();
    if (pin.length !== 6) { $("#pair-status-line").textContent = "Enter the 6-digit PIN."; return; }
    try {
      const status = await invoke("pair_submit_pin", { reqId, pin });
      submitted = true;
      $("#pair-status-line").textContent = status === "verified" ? "Pairing…" : "PIN accepted — waiting for approval…";
    } catch (e) {
      $("#pair-status-line").textContent = String(e);
    }
  }

  stopPairPoll();
  pairPollTimer = setInterval(async () => {
    try {
      const status = await invoke("pair_status", { reqId });
      if (status === "verified") {
        stopPairPoll(); closeModal();
        toast(`Paired with ${peerName}!`);
        refreshDevices();
      } else if (status === "approved") {
        $("#pair-status-line").textContent = "Approved! Now enter the PIN shown on the other device.";
        if (!submitted) $("#pair-pin").focus();
      } else if (status === "denied") {
        stopPairPoll(); closeModal();
        toast("Pairing denied on the other device", true);
      }
    } catch (e) {
      stopPairPoll(); closeModal();
      toast(String(e), true);
    }
  }, 1500);
}

function stopPairPoll() {
  if (pairPollTimer) { clearInterval(pairPollTimer); pairPollTimer = null; }
}

/* -------------------------------------------------------- pairing (target) */

async function showIncomingPair(req) {
  openModal(`
    <h2>Pairing request</h2>
    <p><b>${esc(req.name)}</b> wants to connect to this device.</p>
    <div class="pin-box">${esc(req.pin)}</div>
    <p>Enter this PIN on the other device, then press Allow.</p>
    <div class="row-end">
      <button class="btn danger" id="pair-deny">Deny</button>
      <button class="btn primary" id="pair-allow">Allow</button>
    </div>`);
  $("#pair-allow").addEventListener("click", async () => {
    try { await invoke("pair_approve", { reqId: req.req_id }); closeModal(); refreshDevices(); }
    catch (e) { toast(String(e), true); }
  });
  $("#pair-deny").addEventListener("click", async () => {
    try { await invoke("pair_deny", { reqId: req.req_id }); } catch (e) { toast(String(e), true); }
    closeModal();
  });
}

/* -------------------------------------------------------------------- files */

async function refreshFiles() {
  try {
    state.files.entries = await invoke("browse", {
      peer: state.files.peer,
      path: currentFilesPath(),
    });
    renderFiles();
  } catch (e) {
    toast(String(e), true);
    state.files.entries = [];
    renderFiles();
  }
}

function renderFileTabs() {
  const tabs = [{ id: null, name: "This device" }];
  for (const d of state.devices.filter((x) => x.paired)) {
    tabs.push({ id: d.id, name: d.name });
  }
  if (state.files.peer && !tabs.some((t) => t.id === state.files.peer)) {
    state.files.peer = null;
  }
  $("#file-tabs").innerHTML = tabs.map((t) =>
    `<button class="tab ${(t.id ?? null) === (state.files.peer ?? null) ? "active" : ""}"
      data-peer="${t.id ?? ""}">${esc(t.name)}</button>`).join("");
  $$("#file-tabs .tab").forEach((b) =>
    b.addEventListener("click", () => {
      state.files.peer = b.dataset.peer === "" ? null : b.dataset.peer;
      renderFileTabs();
      refreshFiles();
    }));
}

function renderFiles() {
  const path = currentFilesPath();
  const label = state.files.peer
    ? (state.devices.find((d) => d.id === state.files.peer)?.name ?? "device")
    : "This device";
  $("#files-hint").textContent =
    state.files.peer
      ? `Browsing the shared folder of ${label}. Downloads go to your Downloads folder.`
      : `This is the folder other devices can see. In Settings you can share a different folder.`;

  // breadcrumb
  const parts = path ? path.split("/") : [];
  let acc = "";
  const crumbs = [`<button class="crumb" data-p="">${esc(label)}</button>`];
  for (const part of parts) {
    acc = acc ? acc + "/" + part : part;
    crumbs.push(`<span class="crumb-sep">›</span><button class="crumb" data-p="${esc(acc)}">${esc(part)}</button>`);
  }
  $("#breadcrumb").innerHTML = crumbs.join("");
  $$("#breadcrumb .crumb").forEach((b) =>
    b.addEventListener("click", () => {
      state.files.paths[state.files.peer ?? "local"] = b.dataset.p;
      refreshFiles();
    }));

  const rows = state.files.entries.map((e) => {
    const isLocal = state.files.peer === null;
    const ico = e.is_dir ? "📁" : "📄";
    const click = e.is_dir ? "clickable" : "";
    return `<div class="file-row ${click}" data-path="${esc(e.path)}" data-dir="${e.is_dir ? 1 : 0}">
      <div class="file-ico">${ico}</div>
      <div class="file-name">${esc(e.name)}</div>
      <div class="file-size">${e.is_dir ? "—" : fmtSize(e.size)}</div>
      <div class="file-date">${fmtDate(e.mtime)}</div>
      <div class="file-actions">
        ${!isLocal && !e.is_dir ? `<button class="btn small" data-fa="download">⬇ Download</button>` : ""}
        <button class="btn small" data-fa="rename">Rename</button>
        <button class="btn small danger" data-fa="delete">Delete</button>
      </div>
    </div>`;
  });
  $("#file-list").innerHTML = rows.length
    ? rows.join("")
    : `<div class="empty">Empty folder</div>`;

  $$("#file-list .file-row.clickable").forEach((row) =>
    row.addEventListener("dblclick", () => {
      if (row.dataset.dir === "1") {
        const key = state.files.peer ?? "local";
        state.files.paths[key] = row.dataset.path;
        refreshFiles();
      }
    }));
  $$("#file-list .btn").forEach((b) =>
    b.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const row = b.closest(".file-row");
      fileAction(b.dataset.fa, row.dataset.path, row.dataset.dir === "1");
    }));
}

async function fileAction(act, path, isDir) {
  const peer = state.files.peer;
  try {
    if (act === "download") {
      const tid = await invoke("download_file", { peerId: peer, path });
      toast("Download started");
      updateBadge();
    } else if (act === "delete") {
      if (!confirm(`Delete "${path}"?`)) return;
      if (peer) await invoke("peer_delete", { peerId: peer, path });
      else await invoke("local_delete", { path });
      refreshFiles();
    } else if (act === "rename") {
      const dest = prompt("New name / path:", path.split("/").pop());
      if (!dest) return;
      const destPath = path.includes("/")
        ? path.slice(0, path.lastIndexOf("/") + 1) + dest
        : dest;
      if (peer) await invoke("peer_rename", { peerId: peer, path, dest: destPath });
      else await invoke("local_rename", { path, dest: destPath });
      refreshFiles();
    }
  } catch (e) { toast(String(e), true); }
}

$("#btn-new-folder").addEventListener("click", async () => {
  const name = prompt("Folder name:");
  if (!name) return;
  const path = currentFilesPath() ? currentFilesPath() + "/" + name : name;
  try {
    if (state.files.peer) await invoke("peer_mkdir", { peerId: state.files.peer, path });
    else await invoke("local_mkdir", { path });
    refreshFiles();
  } catch (e) { toast(String(e), true); }
});

$("#btn-upload").addEventListener("click", async () => {
  if (!state.files.peer) {
    toast("Open a paired device first — uploads always go to the other device.", true);
    return;
  }
  try {
    const picked = await dialog().open({ multiple: true });
    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    if (paths.length) {
      await invoke("upload_files", { peerId: state.files.peer, remoteDir: currentFilesPath(), localPaths: paths });
      toast(`Uploading ${paths.length} file(s)…`);
      updateBadge();
    }
  } catch (e) { toast(String(e), true); }
});

$("#btn-refresh-files").addEventListener("click", refreshFiles);

/* ---------------------------------------------------------------- transfers */

function updateBadge() {
  invoke("list_transfers").then((list) => {
    const active = list.filter((t) => t.state === "active").length;
    const badge = $("#transfer-badge");
    badge.classList.toggle("hidden", active === 0);
    badge.textContent = String(active);
  }).catch(() => {});
}

async function renderTransfers() {
  try {
    const list = await invoke("list_transfers");
    $("#transfer-list").innerHTML = list.length
      ? list.map((t) => {
          const pct = t.total > 0 ? Math.min(100, Math.round((t.done / t.total) * 100)) : (t.state === "done" ? 100 : 0);
          const dir = t.kind === "upload" ? "→" : "←";
          return `<div class="transfer-item">
            <div class="transfer-top">
              <div>
                <div class="transfer-name">${dir} ${esc(t.name)}</div>
                <div class="transfer-sub">${esc(t.peer_name)} · ${fmtSize(t.done)} / ${fmtSize(t.total)}</div>
              </div>
              <span class="state-badge ${t.state}">${esc(t.state)}</span>
            </div>
            <div class="progress"><div class="progress-bar" style="width:${pct}%"></div></div>
          </div>`;
        }).join("")
      : `<div class="empty">No transfers yet. Open a paired device in <b>Files</b> and download or upload something.</div>`;
  } catch (e) { toast(String(e), true); }
}

$("#btn-refresh-transfers").addEventListener("click", renderTransfers);

/* ----------------------------------------------------------------- settings */

async function renderSettings() {
  if (!state.info) return;
  $("#set-name").value = state.info.name;
  $("#set-theme").value = state.info.theme;
  $("#set-share").value = state.info.share_dir;
  $("#set-port").value = state.info.port;
  $("#set-automap").checked = state.info.auto_map_drives;
  $("#set-rootmode").checked = state.info.root_mode;

  const isWin = state.info.platform === "windows";
  const isAndroid = state.info.platform === "android";
  $("#panel-windows").classList.toggle("hidden", !isWin);
  $("#panel-android").classList.toggle("hidden", !isAndroid);
  $("#field-automap").classList.toggle("hidden", !isWin);
  $("#field-rootmode").classList.toggle("hidden", !isAndroid);

  if (isWin) {
    try {
      const drives = await invoke("win_list_drives");
      $("#drive-list").innerHTML = drives.length
        ? "Currently mapped: " + drives.map(([l, u]) => `${esc(l)}: → ${esc(u)}`).join(" · ")
        : "No Single drives mapped yet.";
    } catch { /* ignore */ }
  }
}

$("#btn-pick-share").addEventListener("click", async () => {
  try {
    const picked = await dialog().open({ directory: true, multiple: false });
    if (picked) $("#set-share").value = picked;
  } catch (e) { toast(String(e), true); }
});

$("#btn-save-settings").addEventListener("click", async () => {
  try {
    const name = $("#set-name").value.trim();
    const theme = $("#set-theme").value;
    const port = parseInt($("#set-port").value, 10);
    const share = $("#set-share").value.trim();
    const auto = $("#set-automap").checked;
    const root = $("#set-rootmode").checked;
    state.info = await invoke("set_settings", {
      deviceName: name || null,
      theme,
      tcpPort: Number.isFinite(port) ? port : null,
      shareDir: share || null,
      autoMapDrives: auto,
      rootMode: root,
    });
    applyTheme(state.info.theme);
    $("#me-name").textContent = state.info.name;
    toast("Settings saved");
  } catch (e) { toast(String(e), true); }
});

$("#btn-drive-support").addEventListener("click", async () => {
  try {
    await invoke("win_drive_support");
    toast("Drive support enabled. You can map drives now.");
  } catch (e) { toast(String(e), true); }
});

$("#btn-root-setup").addEventListener("click", async () => {
  try {
    const lines = await invoke("android_root_setup");
    $("#root-report").innerHTML = lines.map((l) => `<li>${esc(l)}</li>`).join("");
    toast("Root folders ready");
  } catch (e) { toast(String(e), true); }
});

/* -------------------------------------------------------------------- about */

function renderAbout() {
  const i = state.info ?? {};
  $("#about-grid").innerHTML = `
    <span class="k">Device name</span><span class="v">${esc(i.name ?? "")}</span>
    <span class="k">Device ID</span><span class="v">${esc(i.id ?? "")}</span>
    <span class="k">Addresses</span><span class="v">${esc((i.ips ?? []).join(", ") || "—")}:${esc(i.port ?? "")}</span>
    <span class="k">Shared folder</span><span class="v">${esc(i.share_dir ?? "")}</span>
    <span class="k">Version</span><span class="v">${esc(i.version ?? "")}</span>
    <span class="k">Platform</span><span class="v">${esc(i.platform ?? "")}</span>`;
}

$("#link-repo").addEventListener("click", (e) => {
  e.preventDefault();
  try { T.opener.openUrl("https://github.com/L-Malakar/Single"); } catch { /* mock */ }
});
$("#link-license").addEventListener("click", (e) => {
  e.preventDefault();
  try { T.opener.openUrl("https://github.com/L-Malakar/Single/blob/main/LICENSE"); } catch { /* mock */ }
});

/* ---------------------------------------------------------------- misc ui */

$("#btn-add-manual").addEventListener("click", () => {
  openModal(`
    <h2>Add a device by IP</h2>
    <p>If discovery can't see a device (some routers block it), type its
       address directly. The port is 27500 unless it was changed.</p>
    <div class="field"><span>IP address</span><input id="manual-ip" type="text" placeholder="192.168.1.42" /></div>
    <div class="field"><span>Port</span><input id="manual-port" type="number" value="27500" /></div>
    <div class="row-end">
      <button class="btn ghost" id="manual-cancel">Cancel</button>
      <button class="btn primary" id="manual-ok">Add</button>
    </div>`);
  $("#manual-cancel").addEventListener("click", closeModal);
  $("#manual-ok").addEventListener("click", async () => {
    try {
      const ip = $("#manual-ip").value.trim();
      const port = parseInt($("#manual-port").value, 10) || 27500;
      const name = await invoke("add_manual", { ip, port });
      closeModal();
      toast(`Found ${name}`);
      refreshDevices();
    } catch (e) { toast(String(e), true); }
  });
  $("#manual-ip").focus();
});

$("#btn-refresh-devices").addEventListener("click", refreshDevices);

function applyTheme(theme) {
  document.documentElement.dataset.theme = theme === "light" ? "light" : "dark";
}

/* ------------------------------------------------------------------- boot */

async function boot() {
  try {
    state.info = await invoke("get_app_info");
    applyTheme(state.info.theme);
    $("#me-name").textContent = state.info.name;
    $("#me-id").textContent = `ID ${state.info.id.slice(0, 8)}… · :${state.info.port}`;
    $("#me-version").textContent = state.info.version;
  } catch (e) {
    $("#me-id").textContent = "not connected to backend";
    toast(String(e), true);
  }
  refreshDevices();
  updateBadge();
  setInterval(updateBadge, 4000);

  // core events
  listen("devices-changed", () => { refreshDevices(); });
  listen("pair-incoming", (ev) => showIncomingPair(ev.payload.req));
  listen("pair-resolved", (ev) => {
    if (ev.payload.status === "verified") {
      toast("Device paired!");
      refreshDevices();
      const modalText = $("#modal-box").textContent ?? "";
      if (modalText.includes("Pairing request")) closeModal();
    }
  });
  listen("transfer", (ev) => {
    updateBadge();
    if (state.view === "transfers") renderTransfers();
  });
  listen("toast", (ev) => toast(ev.payload.message, false));
}

boot();
