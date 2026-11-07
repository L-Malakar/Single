# Integration test scripts

- `bridge_e2e_test.py` — end-to-end test of the encrypted files bridge: boots two server instances (mDNS loopback), verifies the E2EE handshake, the `/api/bridge/*` REST surface (list / ranged read / upload / sandbox `..` rejection) and the full WebDAV verb set (OPTIONS / PROPFIND / GET / Range / MKCOL / PUT / DELETE). Needs `pip install websockets`.
- `ws_test.js` — Node WS client: discovery snapshot, connect, handshake steps (incl. the e2ee key exchange), chat round-trip, 1.3 MB file transfer (`npm install ws` first).
- `tcp_inbound_test.py` — raw-socket peer: inbound HELLO/HELLO_ACK/ACK, ping RTT, garbage-frame resilience, bad-magic rejection. No dependencies. Set the server to Public first (the responder pauses in Private mode).
- `access_modes_test.py` — drives the Protocol switch end-to-end over a stdlib WebSocket client: Private accept/decline, Protected wrong/right password, Public instant accept, invalid-mode rejection, `/api/health` reporting. No dependencies.
- `ui_shots.py` — Playwright walkthrough of the dashboard (Home page, sidebar collapse, Protocol UI, Find redirect, rename via the name pill) with screenshots into `shots/`. Needs `playwright` + Chromium.

Run against a live server (`p2p-bridge-server` + `device-simulator`); `access_modes_test.py` and `bridge_e2e_test.py` boot their own servers (ports 3931/5952 and 3001/5061).
