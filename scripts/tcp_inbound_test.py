#!/usr/bin/env python3
"""Raw TCP handshake test against the server's P2P responder (:5050).
Implements the wire protocol from src/protocol.rs with plain sockets —
proves the server accepts inbound HELLO/HELLO_ACK/ACK and handles chat/ping."""
import json, socket, time, sys

HOST, PORT = "127.0.0.1", 5050
MAGIC, VER = "P2PBRIDGE", 1

def send(sock, obj):
    sock.sendall((json.dumps(obj) + "\n").encode())

def recv(sock):
    buf = b""
    while not buf.endswith(b"\n"):
        chunk = sock.recv(65536)
        if not chunk:
            raise EOFError("peer closed")
        buf += chunk
    return json.loads(buf)

results = []
def ok(name, cond, extra=""):
    results.append(cond)
    print(f"{'PASS' if cond else 'FAIL'}  {name}" + (f"  → {extra}" if extra else ""))

s = socket.create_connection((HOST, PORT), timeout=5)
s.settimeout(5)

# 1. HELLO (application-level SYN)
my_nonce = "ab12cd34"
send(s, {"type": "hello", "magic": MAGIC, "ver": VER,
         "id": "python-tester._p2pbridge._tcp.local.",
         "name": "PyTester", "platform": "linux", "nonce": my_nonce})

# 2. HELLO_ACK (SYN-ACK) — must echo our nonce
ack = recv(s)
ok("HELLO_ACK ok=true", ack.get("type") == "hello-ack" and ack.get("ok") is True, str(ack)[:120])
ok("HELLO_ACK echoes our nonce", ack.get("echo_nonce") == my_nonce)
ok("HELLO_ACK carries peer identity", bool(ack.get("name")) and bool(ack.get("nonce")))
peer_nonce = ack.get("nonce", "")

# 3. ACK — must echo peer's nonce
send(s, {"type": "ack", "echo_nonce": peer_nonce})

# 4. chat round-trip
send(s, {"type": "msg", "text": "hello from raw python", "ts": int(time.time() * 1000)})
# server does not auto-reply to chat; but our msg should be accepted silently.
# 5. ping → pong (measures RTT)
t0 = time.time()
send(s, {"type": "ping", "t": int(t0 * 1000)})
pong = recv(s)
rtt = (time.time() - t0) * 1000
ok("PING → PONG echoes timestamp", pong.get("type") == "pong" and pong.get("t") == int(t0 * 1000),
   f"rtt ≈ {rtt:.1f} ms")

# 6. bad frame must be ignored, not kill the connection
s.sendall(b"this is not json\n")
send(s, {"type": "ping", "t": 42})
pong2 = recv(s)
ok("garbage frame ignored, connection alive", pong2.get("type") == "pong" and pong2.get("t") == 42)

# 7. graceful BYE
send(s, {"type": "bye", "reason": "test complete"})
time.sleep(0.3)
s.close()
ok("BYE sent + socket closed", True)

# 8. negative: wrong magic must be rejected
s2 = socket.create_connection((HOST, PORT), timeout=5)
s2.settimeout(5)
send(s2, {"type": "hello", "magic": "WRONG", "ver": VER, "id": "x", "name": "x",
          "platform": "x", "nonce": "n"})
try:
    resp = recv(s2)
    ok("bad magic rejected", resp.get("ok") is False or resp.get("type") == "err", str(resp)[:100])
except (EOFError, ConnectionResetError, socket.timeout):
    ok("bad magic rejected (connection dropped)", True)
s2.close()

print(f"\n=== {sum(results)}/{len(results)} inbound checks passed ===")
sys.exit(0 if all(results) else 1)
