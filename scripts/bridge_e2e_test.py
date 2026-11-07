#!/usr/bin/env python3
"""End-to-end test of the encrypted files bridge.

Boots two p2p-bridge-server instances on one host (mDNS loopback), connects
them through the real WebSocket control channel, then verifies:

  1. handshake completes and the session reports encrypted=true
  2. /api/bridge/list returns the peer's share roots
  3. /api/bridge/file streams the exact file bytes (incl. Range)
  4. /api/bridge/upload pushes a file into the peer's share
  5. WebDAV: OPTIONS, PROPFIND (depth 1), GET, MKCOL, PUT, DELETE
  6. the raw TCP stream carries no plaintext (all frames are sealed)

Usage: python3 bridge_e2e_test.py [path-to-debug-binaries]
"""
import asyncio
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

BIN = sys.argv[1] if len(sys.argv) > 1 else os.path.abspath(
    os.path.join(os.path.dirname(__file__), "..", "p2p-bridge", "target", "debug", "p2p-bridge-server"))

import websockets  # noqa: E402

PASS = 0
FAIL = 0

def ok(name, cond, detail=""):
    global PASS, FAIL
    if cond:
        PASS += 1
        print(f"  ✓ {name}")
    else:
        FAIL += 1
        print(f"  ✗ {name}  {detail}")

def http(method, url, body=None, headers=None, timeout=10):
    req = urllib.request.Request(url, data=body, method=method, headers=headers or {})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            return r.status, r.read(), r.headers  # HTTPMessage: case-insensitive .get
    except urllib.error.HTTPError as e:
        return e.code, e.read(), e.headers

class Client:
    def __init__(self, ws):
        self.ws = ws
        self.msgs = []
    async def next(self, typ, timeout=15):
        deadline = time.time() + timeout
        i = 0
        while i < len(self.msgs):
            m = self.msgs[i]
            if m.get("type") == typ:
                self.msgs.pop(i)
                return m
            i += 1
        while time.time() < deadline:
            try:
                raw = await asyncio.wait_for(self.ws.recv(), timeout=max(0.1, deadline - time.time()))
            except (asyncio.TimeoutError, TimeoutError):
                break
            m = json.loads(raw)
            if m.get("type") == typ:
                return m
            self.msgs.append(m)
        return None

async def main():
    work = tempfile.mkdtemp(prefix="bridge-e2e-")
    share_a = os.path.join(work, "shareA"); os.makedirs(share_a)
    share_b = os.path.join(work, "shareB"); os.makedirs(share_b)
    recv_a = os.path.join(work, "recvA"); recv_b = os.path.join(work, "recvB")

    payload = os.urandom(1_100_000)  # 1.1 MB → exercises multi-chunk reads
    with open(os.path.join(share_b, "hello.txt"), "wb") as f:
        f.write(b"hello from host B\n")
    with open(os.path.join(share_b, "big.bin"), "wb") as f:
        f.write(payload)

    env = dict(os.environ)
    procs = []
    def spawn(args):
        p = subprocess.Popen([BIN] + args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                             env=env, cwd=work, text=True)
        procs.append(p)
        return p

    # --- host A: public mode so B's inbound test also works ----------------
    spawn(["--name", "HostA", "--http-port", "3001", "--p2p-port", "5061", "--dav-port", "5071",
           "--mdns-loopback", "--no-open", "--received-dir", recv_a, "--share", share_a])
    # --- host B: public mode (no approval dialog needed) -------------------
    spawn(["--name", "HostB", "--http-port", "3002", "--p2p-port", "5062", "--dav-port", "5072",
           "--mdns-loopback", "--no-open", "--received-dir", recv_b, "--share", share_b])

    def wait_health(port, name):
        for _ in range(80):
            try:
                s, _, _ = http("GET", f"http://127.0.0.1:{port}/api/health", timeout=1)
                if s == 200:
                    return True
            except Exception:
                pass
            time.sleep(0.25)
        print(f"  !! {name} never became healthy"); return False

    print("— booting two instances —")
    ok("HostA healthy", wait_health(3001, "HostA"))
    ok("HostB healthy", wait_health(3002, "HostB"))

    try:
        async with websockets.connect("ws://127.0.0.1:3001/ws") as wsa, \
                   websockets.connect("ws://127.0.0.1:3002/ws") as wsb:
            ca, cb = Client(wsa), Client(wsb)

            # set both to public so the handshake needs no approval
            await wsa.send(json.dumps({"type": "set-access", "mode": "public"}))
            await wsb.send(json.dumps({"type": "set-access", "mode": "public"}))

            # wait for HostB to appear in A's registry (mDNS loopback)
            print("— mDNS discovery over loopback —")
            dev_b = None
            for _ in range(40):
                await wsa.send(json.dumps({"type": "refresh"}))
                devs = await ca.next("devices", timeout=3)
                if devs:
                    dev_b = next((d for d in devs["devices"] if d["name"] == "HostB"), None)
                    if dev_b:
                        break
                await asyncio.sleep(1)
            ok("HostB discovered by HostA", dev_b is not None, str(devs and [d['name'] for d in devs['devices']]))

            # connect A → B
            print("— P2P handshake + E2EE —")
            await wsa.send(json.dumps({"type": "connect", "device_id": dev_b["id"]}))
            sess = None
            for _ in range(30):
                s = await ca.next("session", timeout=3)
                if not s:
                    continue
                if s["session"]["state"] == "established":
                    sess = s["session"]
                    break
                if s["session"]["state"] == "error":
                    break
            ok("session established", sess is not None, str(sess and sess.get("error")))
            ok("session reports encrypted", bool(sess and sess.get("encrypted")))

            # raw TCP must not contain the plaintext of a chat message
            print("— transport is really encrypted —")
            marker = "SECRET-e2ee-marker-" + os.urandom(4).hex()
            await wsa.send(json.dumps({"type": "chat", "device_id": dev_b["id"], "text": marker}))
            await asyncio.sleep(1.0)
            tcpdump = subprocess.run(
                ["timeout", "2", "tcpdump", "-i", "lo", "-A", "-s", "0", "port", "5062"],
                capture_output=True, text=True)
            if tcpdump.returncode in (0, 124):
                body = tcpdump.stdout
                ok("chat text invisible on the wire", marker not in body)
                ok("frames sealed (type e visible)", '"type":"e"' in body or '"type": "e"' in body)
            else:
                print("  (tcpdump unavailable — skipping wire capture check)")

            # bridge status via WS — the loop only pushes on CHANGE, so wait
            # for a snapshot that actually shows HostB established (≤ ~10 s).
            print("— /api/bridge + WebDAV —")
            status = None
            for _ in range(6):
                m = await ca.next("bridge", timeout=3)
                if m and any(p["peer_name"] == "HostB" and p["state"] == "established"
                             for p in m["bridge"]["peers"]):
                    status = m
                    break
                if not m:
                    break
            ok("bridge snapshot has peer HostB", status is not None and any(
                p["peer_name"] == "HostB" and p["state"] == "established" for p in status["bridge"]["peers"]))

            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/status")
            st = json.loads(body)
            ok("bridge status REST", s == 200 and any(p["peer_name"] == "HostB" for p in st["peers"]))

            # list B's share roots from A
            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/list?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/")
            listing = json.loads(body)
            ok("list / returns share roots", s == 200 and listing.get("ok") and
               any(e["name"] == "shareB" for e in listing["entries"]), body[:200])
            root_title = next((e["name"] for e in listing["entries"]), None)

            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/list?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/" + root_title)
            listing = json.loads(body)
            names = [e["name"] for e in listing.get("entries", [])]
            ok("list share contents", "hello.txt" in names and "big.bin" in names, str(names))

            # read the whole small file
            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/file?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/" + root_title + "/hello.txt")
            ok("read hello.txt over bridge", s == 200 and body == b"hello from host B\n", repr(body[:60]))

            # read 1.1 MB file and compare bytes
            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/file?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/" + root_title + "/big.bin")
            ok("big.bin roundtrip (1.1 MB, multi-chunk)", s == 200 and body == payload,
               f"len={len(body)}")

            # range request
            s, body, hdr = http("GET", "http://127.0.0.1:3001/api/bridge/file?peer=" +
                                urllib.parse.quote(dev_b["id"]) + "&path=/" + root_title + "/big.bin",
                                headers={"Range": "bytes=100-199"})
            ok("range request", s == 206 and body == payload[100:200] and
               str(hdr.get("Content-Range", "")).startswith("bytes 100-199/"),
               f"{s} {hdr.get('Content-Range')} len={len(body)}")

            # sandbox escape attempt
            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/list?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/shareB/../../etc")
            ok("sandbox blocks ..", json.loads(body).get("ok") is False, body[:120])

            # upload A → B
            up = os.urandom(300_000)
            s, body, _ = http("PUT", "http://127.0.0.1:3001/api/bridge/upload?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/" + root_title + "/fromA.bin",
                              body=up, headers={"Content-Type": "application/octet-stream"})
            ok("upload via /api/bridge/upload", s == 200 and json.loads(body).get("ok"), body[:200])
            ok("uploaded bytes stored on B",
               os.path.isfile(os.path.join(share_b, "fromA.bin")) and
               open(os.path.join(share_b, "fromA.bin"), "rb").read() == up)

            # ---- WebDAV (as Windows would) ----
            dav = "http://127.0.0.1:5071/dav/HostB"
            s, body, hdr = http("OPTIONS", dav + "/")
            ok("WebDAV OPTIONS", s == 200 and "1, 2" in hdr.get("DAV", ""), str(hdr.get("DAV")))

            s, body, _ = http("PROPFIND", dav + "/", headers={"Depth": "1"})
            ok("PROPFIND depth 1 (root)", s == 207 and b"shareB" in body and b"href" in body, body[:160])

            s, body, _ = http("PROPFIND", dav + "/shareB/", headers={"Depth": "1"})
            ok("PROPFIND lists files", s == 207 and b"hello.txt" in body and b"big.bin" in body, body[:160])

            s, body, _ = http("GET", dav + "/shareB/hello.txt")
            ok("WebDAV GET", s == 200 and body == b"hello from host B\n")

            s, body, _ = http("GET", dav + "/shareB/big.bin",
                              headers={"Range": "bytes=0-1023"})
            ok("WebDAV ranged GET", s == 206 and body == payload[:1024])

            s, body, _ = http("MKCOL", dav + "/shareB/newfolder")
            ok("WebDAV MKCOL", s in (200, 201) and os.path.isdir(os.path.join(share_b, "newfolder")))

            s, body, _ = http("PUT", dav + "/shareB/newfolder/via-dav.txt",
                              body=b"dav upload works\n")
            ok("WebDAV PUT", s in (200, 201))
            ok("WebDAV PUT stored",
               open(os.path.join(share_b, "newfolder", "via-dav.txt"), "rb").read() == b"dav upload works\n")

            s, body, _ = http("PROPFIND", dav + "/shareB/newfolder/via-dav.txt", headers={"Depth": "0"})
            ok("PROPFIND depth 0", s == 207 and b"via-dav.txt" in body, body[:160])

            s, body, _ = http("DELETE", dav + "/shareB/newfolder/via-dav.txt")
            ok("WebDAV DELETE", s in (200, 204) and not os.path.exists(os.path.join(share_b, "newfolder", "via-dav.txt")))

            # unknown peer slug → 404
            s, _, _ = http("PROPFIND", "http://127.0.0.1:5071/dav/NobodyHere/")
            ok("unknown device slug → 404", s == 404)

            # loopback guard on the REST bridge (WebDAV has no guard needed: loopback bind)
            # → can't easily test a non-loopback source here; bind check is structural.

            # disconnect
            await wsa.send(json.dumps({"type": "disconnect", "session_id": dev_b["id"]}))
            await asyncio.sleep(0.8)
            s, body, _ = http("GET", "http://127.0.0.1:3001/api/bridge/list?peer=" +
                              urllib.parse.quote(dev_b["id"]) + "&path=/")
            ok("bridge list fails after disconnect", json.loads(body).get("ok") is False)

    finally:
        print("— shutting down —")
        for p in procs:
            p.send_signal(signal.SIGINT)
        time.sleep(1.0)
        for p in procs:
            if p.poll() is None:
                p.kill()
        for p in procs:
            out = p.stdout.read() if p.stdout else ""
            for line in out.splitlines():
                if "error" in line.lower() or "panic" in line.lower():
                    print("   [srv]", line[:160])
        shutil.rmtree(work, ignore_errors=True)

    print(f"\n==== {PASS} passed, {FAIL} failed ====")
    sys.exit(1 if FAIL else 0)

asyncio.run(main())
