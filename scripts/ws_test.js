// Integration test: WS control channel → connect → handshake → chat → file transfer
const WebSocket = require("ws");
const crypto = require("crypto");

const ws = new WebSocket("ws://localhost:3000/ws");
const seen = { hello: false, devices: false };
let pixelId = null;
const results = [];
const ok = (name, cond, extra) => {
  results.push({ name, pass: !!cond, extra: extra || "" });
  console.log(`${cond ? "PASS" : "FAIL"}  ${name}${extra ? "  → " + extra : ""}`);
};

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

ws.on("open", () => console.log("ws open"));
ws.on("error", (e) => { console.error("ws error:", e.message); process.exit(1); });

let established = false;
let chatReply = null;
let fileDone = null;
let sessionMsg = null;
let devicesMsg = null;

ws.on("message", (raw) => {
  const msg = JSON.parse(raw.toString());
  switch (msg.type) {
    case "hello":
      seen.hello = true;
      ok("hello: identity + service type", msg.self_identity.name === "dev-server" && msg.service_type === "_p2pbridge._tcp.local.", JSON.stringify(msg.self_identity));
      break;
    case "devices":
      devicesMsg = msg.devices;
      if (!seen.devices) {
        seen.devices = true;
        ok("devices: 5 simulated devices discovered", msg.devices.length === 5, `${msg.devices.length} devices: ${msg.devices.map(d => d.name).join(", ")}`);
        const pixel = msg.devices.find((d) => d.name === "Pixel 8" && d.platform === "android");
        if (pixel) {
          pixelId = pixel.id;
          console.log("→ sending connect for", pixel.id);
          ws.send(JSON.stringify({ type: "connect", device_id: pixel.id }));
        } else {
          ok("Pixel 8 present in registry", false);
        }
      }
      break;
    case "session":
      sessionMsg = msg.session;
      if (msg.session.state === "established" && msg.session.peer_id === pixelId) established = true;
      break;
    case "chat":
      if (msg.peer_id === pixelId && msg.line.dir === "in") chatReply = msg.line.text;
      break;
    case "file":
      if (msg.event && msg.event.status === "done" && msg.event.direction === "out") fileDone = msg.event;
      break;
    case "log":
      console.log(`  [srv ${msg.level}] ${msg.msg}`);
      break;
  }
});

(async () => {
  // wait for discovery + connect
  for (let i = 0; i < 60 && !established; i++) await sleep(500);
  ok("handshake: session ESTABLISHED with Pixel 8", established,
     sessionMsg ? `state=${sessionMsg.state} steps=[${sessionMsg.steps.map(s => s.step).join(",")}]` : "no session msg");
  if (sessionMsg) {
    ok("handshake: 5 steps (connect/hello/hello-ack/ack/established)",
       ["connect", "hello", "hello-ack", "ack", "established"].every(s => sessionMsg.steps.some(x => x.step === s)),
       sessionMsg.steps.map(s => s.step).join(" → "));
    ok("handshake: TCP connect ms recorded", sessionMsg.steps[0].ms > 0, sessionMsg.steps[0].ms + " ms");
  }

  // chat round-trip
  if (established) {
    ws.send(JSON.stringify({ type: "chat", device_id: pixelId, text: "integration-check" }));
    for (let i = 0; i < 30 && !chatReply; i++) await sleep(200);
    ok("chat: echo bot replied", chatReply != null, chatReply);
  }

  // file transfer round-trip (~1.3 MB)
  if (established) {
    const size = 1_350_000;
    const payload = crypto.randomBytes(size);
    const sha = crypto.createHash("sha256").update(payload).digest("hex");
    const tid = "itest" + Date.now();
    ws.send(JSON.stringify({ type: "file-begin", device_id: pixelId, tid, name: "integration.bin", size, sha256: sha }));
    const CH = 48 * 1024;
    let seq = 0;
    for (let off = 0; off < payload.length; off += CH, seq++) {
      const chunk = payload.subarray(off, Math.min(off + CH, payload.length));
      ws.send(JSON.stringify({ type: "file-chunk", device_id: pixelId, tid, seq, data: chunk.toString("base64") }));
      await sleep(3);
    }
    ws.send(JSON.stringify({ type: "file-end", device_id: pixelId, tid }));
    for (let i = 0; i < 60 && !fileDone; i++) await sleep(500);
    ok("file: 1.3 MB transfer done + sha256 verified by peer", fileDone != null, fileDone ? fileDone.detail : "timeout");

    // ping/pong RTT
    ws.send(JSON.stringify({ type: "ping" }));
    await sleep(500);
  }

  // summary
  const pass = results.filter(r => r.pass).length;
  console.log(`\n=== ${pass}/${results.length} checks passed ===`);
  const failed = results.filter(r => !r.pass);
  process.exit(failed.length ? 1 : 0);
})();
