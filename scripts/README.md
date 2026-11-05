# Integration test scripts

- `ws_test.js` — Node WS client: discovery snapshot, connect, handshake steps, chat round-trip, 1.3 MB file transfer (`npm install ws` first).
- `tcp_inbound_test.py` — raw-socket peer: inbound HELLO/HELLO_ACK/ACK, ping RTT, garbage-frame resilience, bad-magic rejection. No dependencies.
- `access_modes_test.py` — drives the Protocol switch end-to-end over a stdlib WebSocket client: Private accept/decline, Protected wrong/right password, Public instant accept, invalid-mode rejection, `/api/health` reporting. No dependencies.
- `ui_shots.py` — Playwright walkthrough of the dashboard (Home page, sidebar collapse, Protocol UI, Find redirect, rename via the name pill) with screenshots into `shots/`. Needs `playwright` + Chromium.

Run against a live server (`p2p-bridge-server` + `device-simulator`); `access_modes_test.py` boots its own server on ports 3931/5951.
