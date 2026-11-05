# Integration test scripts

- `ws_test.js` — Node WS client: discovery snapshot, connect, handshake steps, chat round-trip, 1.3 MB file transfer (`npm install ws` first).
- `tcp_inbound_test.py` — raw-socket peer: inbound HELLO/HELLO_ACK/ACK, ping RTT, garbage-frame resilience, bad-magic rejection. No dependencies.

Run against a live server (`p2p-bridge-server` + `device-simulator`).
