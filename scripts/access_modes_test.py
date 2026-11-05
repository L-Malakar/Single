#!/usr/bin/env python3
"""End-to-end test for the Protocol (access-control) feature.

Boots p2p-bridge-server on loopback-only-ish flags and drives it through:
  1. Private (default): HELLO -> connect-request on WS -> accept  => ok:true
  2. Private: HELLO -> connect-request -> decline               => ok:false "declined"
  3. Protected (password "pw123"): HELLO w/o password           => ok:false "auth"
  4. Protected: HELLO wrong password                            => ok:false "auth"
  5. Protected: HELLO correct password                          => ok:true
  6. Public: HELLO without password, no request prompt          => ok:true
  7. set-access with unknown mode                               => rejected (mode stays)
  8. /api/health reports access_mode
"""

import base64, hashlib, json, os, socket, struct, subprocess, sys, time, urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "p2p-bridge-server"

HTTP_PORT, P2P_PORT = 3931, 5951
WS_URL_HOST = f"127.0.0.1:{HTTP_PORT}"
PASSWORD = "pw123"

results = []

def check(name, cond, extra=""):
    results.append((name, bool(cond), extra))
    print(("PASS " if cond else "FAIL ") + name + (f"  [{extra}]" if extra and not cond else ""))

# ---------------------------------------------------------------- ws client
class WS:
    def __init__(self, host, port, path="/ws"):
        self.sock = socket.create_connection((host, port), timeout=10)
        key = base64.b64encode(os.urandom(16)).decode()
        req = (f"GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\n"
               "Upgrade: websocket\r\nConnection: Upgrade\r\n"
               f"Sec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n")
        self.sock.sendall(req.encode())
        buf = b""
        while b"\r\n\r\n" not in buf:
            buf += self.sock.recv(4096)
        assert b"101" in buf.split(b"\r\n")[0], buf[:200]
        self.buf = buf.split(b"\r\n\r\n", 1)[1]

    def _read(self, n):
        while len(self.buf) < n:
            chunk = self.sock.recv(4096)
            if not chunk:
                raise ConnectionError("ws closed")
            self.buf += chunk
        out, self.buf = self.buf[:n], self.buf[n:]
        return out

    def recv_msg(self, timeout=10):
        self.sock.settimeout(timeout)
        while True:
            b1, b2 = self._read(2)
            opcode, masked, ln = b1 & 0x0F, b2 & 0x80, b2 & 0x7F
            if ln == 126:
                ln = struct.unpack(">H", self._read(2))[0]
            elif ln == 127:
                ln = struct.unpack(">Q", self._read(8))[0]
            mask = self._read(4) if masked else b""
            payload = self._read(ln)
            if mask:
                payload = bytes(c ^ mask[i % 4] for i, c in enumerate(payload))
            if opcode == 1:
                return json.loads(payload.decode())
            if opcode == 8:
                raise ConnectionError("ws close frame")

    def send_msg(self, obj):
        data = json.dumps(obj).encode()
        mask = os.urandom(4)
        header = bytes([0x81])
        n = len(data)
        if n < 126:
            header += bytes([0x80 | n])
        elif n < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", n)
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", n)
        masked = bytes(c ^ mask[i % 4] for i, c in enumerate(data))
        self.sock.sendall(header + mask + masked)

# ---------------------------------------------------------------- p2p client
def p2p_hello(password="", timeout=8):
    """Open TCP to the P2P port, send HELLO, return (socket, file, ack dict|None)."""
    s = socket.create_connection(("127.0.0.1", P2P_PORT), timeout=timeout)
    s.settimeout(timeout)
    hello = {"type": "hello", "magic": "P2PBRIDGE", "ver": 1, "id": "tester._p2pbridge._tcp.local.",
             "name": "Py Tester", "platform": "linux",
             "nonce": "n-" + os.urandom(4).hex()}
    if password:
        hello["password"] = password
    s.sendall((json.dumps(hello) + "\n").encode())
    return s, hello

def p2p_ack(s, timeout=8):
    s.settimeout(timeout)
    f = s.makefile("rwb")
    try:
        line = f.readline()
    except (socket.timeout, TimeoutError):
        return None
    if not line:
        return None
    return json.loads(line.decode())

def p2p_establish(s, hello, ack):
    s.sendall((json.dumps({"type": "ack", "echo_nonce": ack["nonce"]}) + "\n").encode())

def drain_ws(ws, seconds):
    """Collect every WS message arriving within the window."""
    out = []
    end = time.time() + seconds
    while time.time() < end:
        try:
            out.append(ws.recv_msg(timeout=max(0.05, end - time.time())))
        except (socket.timeout, TimeoutError):
            break
        except ConnectionError:
            break
    return out

def expect_request(ws):
    """Wait for a connect-request message (up to 6 s)."""
    end = time.time() + 6
    while time.time() < end:
        try:
            m = ws.recv_msg(timeout=max(0.05, end - time.time()))
        except (socket.timeout, TimeoutError):
            return None
        if m.get("type") == "connect-request":
            return m
    return None

def skip_to_ack(ws, seconds=8):
    """Read WS noise until nothing left (keeps buffer clean)."""
    drain_ws(ws, 0.3)

def main():
    root = "/tmp/acc-test"
    subprocess.run(["rm", "-rf", root], check=False)
    os.makedirs(root + "/received", exist_ok=True)
    server = subprocess.Popen(
        [str(SERVER),
         "--name", "acc-test-host", "--http-port", str(HTTP_PORT),
         "--p2p-port", str(P2P_PORT), "--no-announce", "--no-open",
         "--received-dir", root + "/received"],
        stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
    try:
        # wait for the HTTP port
        for _ in range(60):
            try:
                urllib.request.urlopen(f"http://{WS_URL_HOST}/api/health", timeout=1)
                break
            except Exception:
                time.sleep(0.25)
        else:
            check("server booted", False, "http never came up")
            return

        # 8. health before anything: default mode
        h = json.load(urllib.request.urlopen(f"http://{WS_URL_HOST}/api/health", timeout=3))
        check("health: default mode is private", h.get("access_mode") == "private", str(h.get("access_mode")))

        ws = WS("127.0.0.1", HTTP_PORT)
        initial = drain_ws(ws, 1.0)
        kinds = [m.get("type") for m in initial]
        check("ws snapshot includes hello/devices/access-state",
              all(k in kinds for k in ("hello", "devices", "access-state")), str(kinds))
        st = next(m for m in initial if m["type"] == "access-state")
        check("access-state default private", st.get("mode") == "private", str(st))

        # ---- 1. Private: accept ------------------------------------------
        s1, hello1 = p2p_hello()
        ack1 = p2p_ack(s1, timeout=2)               # must NOT arrive quickly
        check("private: no ack before approval", ack1 is None, str(ack1))
        req = expect_request(ws)
        check("private: connect-request emitted",
              req and req.get("name") == "Py Tester", str(req))
        if req:
            ws.send_msg({"type": "connect-reply", "id": req["id"], "accept": True})
            ack1 = p2p_ack(s1)
            check("private: accepted => ok:true",
                  ack1 and ack1.get("ok") is True, str(ack1))
            if ack1 and ack1.get("ok"):
                p2p_establish(s1, hello1, ack1)
                msgs = drain_ws(ws, 0.6)
                sess = next((m for m in msgs if m.get("type") == "session" and
                             m["session"]["state"] == "established"), None)
                check("private: session established broadcast", sess is not None)
                s1.sendall((json.dumps({"type": "bye", "reason": "test done"}) + "\n").encode())
        s1.close()
        skip_to_ack(ws)

        # ---- 2. Private: decline ------------------------------------------
        s2, _ = p2p_hello()
        req2 = expect_request(ws)
        check("private: second request emitted", req2 is not None)
        if req2:
            ws.send_msg({"type": "connect-reply", "id": req2["id"], "accept": False})
            ack2 = p2p_ack(s2)
            check("private: declined => ok:false reason 'declined'",
                  ack2 and ack2.get("ok") is False and ack2.get("reason") == "declined", str(ack2))
        s2.close()
        skip_to_ack(ws)

        # ---- 3-5. Protected ------------------------------------------------
        ws.send_msg({"type": "set-access", "mode": "protected", "password": PASSWORD})
        st2 = next((m for m in drain_ws(ws, 1.0) if m.get("type") == "access-state"), None)
        check("protected: access-state pushed (has_password)",
              st2 and st2.get("mode") == "protected" and st2.get("has_password") is True, str(st2))
        h = json.load(urllib.request.urlopen(f"http://{WS_URL_HOST}/api/health", timeout=3))
        check("health reflects protected", h.get("access_mode") == "protected", str(h.get("access_mode")))

        s3, _ = p2p_hello()                              # no password
        ack3 = p2p_ack(s3)
        check("protected: no password => reason 'auth'",
              ack3 and not ack3.get("ok") and ack3.get("reason") == "auth", str(ack3))
        s3.close()

        s4, _ = p2p_hello(password="wrong")              # wrong password
        ack4 = p2p_ack(s4)
        check("protected: wrong password => reason 'auth'",
              ack4 and not ack4.get("ok") and ack4.get("reason") == "auth", str(ack4))
        s4.close()

        s5, hello5 = p2p_hello(password=PASSWORD)        # correct
        ack5 = p2p_ack(s5)
        check("protected: correct password => ok:true", ack5 and ack5.get("ok") is True, str(ack5))
        if ack5 and ack5.get("ok"):
            p2p_establish(s5, hello5, ack5)
        s5.close()
        skip_to_ack(ws)

        # ---- 6. Public ------------------------------------------------------
        ws.send_msg({"type": "set-access", "mode": "public", "password": ""})
        st3 = next((m for m in drain_ws(ws, 1.0) if m.get("type") == "access-state"), None)
        check("public: access-state pushed", st3 and st3.get("mode") == "public", str(st3))
        s6, hello6 = p2p_hello()
        ack6 = p2p_ack(s6, timeout=2)                    # immediate, no approval
        check("public: instant ok:true (no request)",
              ack6 and ack6.get("ok") is True, str(ack6))
        reqs = expect_request(ws)
        check("public: no connect-request emitted", reqs is None)
        if ack6 and ack6.get("ok"):
            p2p_establish(s6, hello6, ack6)
        s6.close()

        # ---- 7. invalid mode rejected ---------------------------------------
        ws.send_msg({"type": "set-access", "mode": "banana", "password": ""})
        st4 = next((m for m in drain_ws(ws, 1.0) if m.get("type") == "access-state"), None)
        check("invalid mode ignored (stays public)",
              st4 and st4.get("mode") == "public", str(st4))

        ws.sock.close()
    finally:
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()

    failed = [r for r in results if not r[1]]
    print(f"\n=== {len(results) - len(failed)}/{len(results)} checks passed ===")
    sys.exit(1 if failed else 0)

if __name__ == "__main__":
    main()
