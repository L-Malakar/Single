# ⇄ Single — mDNS LAN Control Server

> **App name: Single** (Rust crate: `p2p-bridge`). `build.bat` packages it as a
double-clickable Windows app — see [Packaging](#packaging-the-windows-app).

A local Wi-Fi cross-platform control server written in Rust. It **broadcasts itself
over mDNS** (Bonjour / Zeroconf), **discovers Android & Windows peers** on the
network, and serves an **embedded WebSocket dashboard** from which you can run a
**peer-to-peer TCP handshake** with any device — then chat, measure latency, and
transfer files directly over the P2P channel.

```
┌────────────┐   mDNS 224.0.0.251:5353   ┌──────────────┐
│  Android   │◄──────────────────────────►│   Windows    │
│  device    │   _p2pbridge._tcp.local.   │   desktop    │
└─────┬──────┘                            └──────┬───────┘
      │            P2P TCP handshake             │
      │  HELLO → HELLO_ACK → ACK (nonce echo)    │
      │◄────────────  direct TCP  ──────────────►│
      │  chat · PING/PONG RTT · file transfer    │
                    ▲                ▲
            ┌───────┴────────────────┴───────┐
            │   p2p-bridge-server (this)     │
            │  Axum + Tokio + mdns-sd        │
            │  WS dashboard on :3000         │
            │  P2P TCP listener on :5050     │
            └───────────────┬────────────────┘
                            │ WebSocket (live push)
                     ┌──────┴──────┐
                     │  browser    │
                     │  dashboard  │
                     └─────────────┘
```

---

## Quick start

Requires Rust 1.75+ (stable).

```bash
# 0 — or skip all of this: package the double-clickable Windows app
#     build.bat      ->  Single\Single-v0.1.1.exe  +  Single-v0.1.1.zip

# 1 — build both binaries
cargo build

# 2 — terminal A: start the control server
./target/debug/p2p-bridge-server --name my-laptop

# 3 — terminal B (optional, for a solo demo): fake some Android/Windows peers
./target/debug/device-simulator

# 4 — open the dashboard
#    http://localhost:3000
#    (the exe opens this window by itself — no address to type)
```

The simulator registers five fake devices over **real mDNS** —
`Pixel 8`, `Galaxy Tab S9` (Android), `DESKTOP-WIN11-4F2A`, `Surface Pro 11`
(Windows) and `OnePlus 9 (flaky)`, which periodically drops off Wi-Fi and
rejoins to demonstrate goodbye-packet removal and rediscovery. Staggered
registration lets you watch them pop into the dashboard one by one.

On a real Wi-Fi, simply run `p2p-bridge-server` on every machine you want to
see — each instance broadcasts and browses the same service type.

### The dashboard

The UI is a multi-page app in the spirit of LocalSend: a left navigation rail
(collapse it to icons with the chevron button; while collapsed, the app logo
itself expands it back — YouTube-style) with five
focused pages instead of one dense control panel.

| page | what lives there |
| --- | --- |
| **Home** | LocalSend-style landing: big app logo, your device name (tap to rename in Settings), a **Find devices** button, and the **Protocol** switch |
| **Devices** | *this device* identity card, the nearby-device grid (mDNS), platform filters, Connect |
| **Chats** | conversation list + one conversation per device: bubbles, RTT badge, file transfers, collapsible handshake timeline |
| **Activity** | uptime/devices/sessions stats and the live server log with level filters |
| **Settings** | device name (rename), dark/light/system theme, network info, firewall help, about |

### Protocol — inbound connection policy

The Home page's Protocol switch (Public / Private / Protected, default
**Private**) decides how other devices may open a P2P channel with you:

- **Public** — any device on the Wi-Fi connects directly.
- **Private** — every inbound handshake pauses and the dashboard pops an
  *Accept / Decline* dialog; no answer within 30 s counts as a decline. The
  initiating side simply sees the handshake wait, then succeed or fail.
- **Protected** — any device may connect, but its HELLO frame must carry the
  password you set. A wrong/missing password is rejected with `reason:"auth"`,
  and the initiating dashboard pops a password dialog and retries.

The choice is stored in the browser (localStorage) and re-applied to the
server on every dashboard reconnect; the password itself lives in server
memory only.

### Demo walkthrough

1. **Devices appear** on the Devices page as mDNS resolves them (platform badge
   comes from the `platform=` TXT record). Filter with the All / Android /
   Windows / Other chips. (Land on **Home** first — tap *Find devices*.)
2. Click **Connect** — you are taken to Chats, a conversation opens, and the
   handshake timeline replays while connecting:
   `TCP connect (kernel SYN/SYN-ACK/ACK) → HELLO → HELLO_ACK → ACK → ESTABLISHED`,
   with per-step timings. It auto-collapses once established (reopen with the
   *handshake* toggle).
3. Once established: **chat** (bubbles, Enter to send), **RTT** badge
   (PING/PONG), and **📎 file transfer** (48 KB base64 chunks, SHA-256
   verified end-to-end; received files land in `received/`). Unread messages
   badge the Chats nav item.
4. Rename your device in **Settings → Identity** — the server un-registers and
   re-registers its mDNS service; peers see you leave and rejoin under the new
   name.
5. Every mDNS event, handshake and transfer streams into **Activity**.

---

## How discovery works

- Service type: `_p2pbridge._tcp.local.` (RFC 6762/6763 — UDP multicast 224.0.0.251:5353)
- TXT record: `platform=android|windows|linux|macos`, `proto=P2PBRIDGE/1`
- The `mdns-sd` crate (pure Rust — **no Avahi/Bonjour daemon needed**) handles
  both announcing and browsing.
- Browsing runs a 15 s restart cycle (mdns-sd retransmission backs off
  exponentially) and a TTL reaper removes devices silent for 45 s, so the
  device list always reflects reality. mDNS goodbye packets remove devices
  instantly.

**Android/Windows peers** run the same program (it cross-compiles to Windows,
macOS, Linux and Android) or any client implementing the wire protocol below —
e.g. a small Kotlin app using `NsdManager` to register the service and a plain
`Socket` for the handshake.

## The P2P handshake

Newline-delimited JSON over a direct TCP connection to the peer's advertised
port (default 5050). Nonces are generated by each side and echoed back — the
same idea as TCP sequence numbers:

```
initiator ──TCP connect (kernel 3-way)──▶ responder
initiator ──HELLO  {magic:"P2PBRIDGE", ver:1, id, name, platform, nonce,
                    password?}──────────────────────────▶
initiator ◀──HELLO_ACK {ok, id, name, platform, echo_nonce, nonce,
                        reason? if rejected}── responder
initiator ──ACK {echo_nonce: responder.nonce}──▶
                         ESTABLISHED
   then: msg / ping / pong / file-start / file-chunk / file-progress /
         file-end / file-done / bye / err          (see src/protocol.rs)
```

`password` is only used when the responder runs in **Protected** mode.
`reason` explains a rejection: `auth` (wrong/missing password), `declined`
(Private mode, operator said no) or `timeout` (nobody answered the approval
dialog within 30 s).

Frames and the WebSocket control-channel schema are documented inline in
[`src/protocol.rs`](src/protocol.rs) — the single source of truth for both
ends.

## Packaging the Windows app

`build.bat` (run it on Windows, from the project root) compiles the release
binaries and builds a distributable folder — no installer, no localhost
address to type:

```
Single/
├── Single-v0.1.1.exe      the app — double-click it: the console is the
│                          server, and the dashboard opens automatically
│                          as an Edge/Chrome --app window
├── Launch Single.bat      friendly starter
├── enable-firewall.bat    one-time firewall rules (mDNS / dashboard / P2P),
│                          self-elevating via UAC
├── README.txt             end-user notes
├── received/              inbound transfers land here
└── tools/
    └── Device-Simulator.exe

Single-v0.1.1.zip          the same folder, zipped for sharing
```

The version is read from `Cargo.toml` automatically — bump it there, re-run
`build.bat`, done. The exe itself gains two app-mode behaviours: it switches
the console to UTF-8, and once the HTTP listener is up it opens the dashboard
in an app-style window (Edge → Chrome → default browser; disable with
`--no-open`).

## CLI reference

**`p2p-bridge-server`**

| flag | default | meaning |
| --- | --- | --- |
| `--name <n>` | hostname | display name broadcast over mDNS |
| `--http-port <p>` | 3000 | dashboard + WebSocket port |
| `--p2p-port <p>` | 5050 | peer-to-peer TCP listener |
| `--received-dir <dir>` | `./received` | where inbound files are stored |
| `--static-dir <dir>` | embedded | serve the UI from disk (live-edit without rebuilds) |
| `--no-open` | on | don't auto-open the dashboard app window |
| `--no-announce` | — | browse only, don't broadcast ourselves |
| `--mdns-loopback` | off | also join multicast on `lo` (containers / offline dev) |

**`device-simulator`**

| flag | default | meaning |
| --- | --- | --- |
| `--base-port <p>` | 5100 | TCP port of the first fake device (then +1 each) |
| `--no-flap` | — | disable the flaky Wi-Fi device |
| `--duration <secs>` | forever | auto-exit after N seconds |
| `--ip <a.b.c.d>` | auto | override the advertised address |
| `--received-dir <dir>` | `./received-sim` | where simulated devices store files |

`GET /api/health` returns a JSON status probe (uptime, device count, sessions).

## Ports & firewall

| port | protocol | purpose |
| --- | --- | --- |
| 5353 | UDP multicast 224.0.0.251 | mDNS discovery (must not be blocked) |
| 3000 | TCP | HTTP dashboard + WebSocket |
| 5050 | TCP | P2P handshake / chat / file channel |

Windows (admin PowerShell):

```powershell
New-NetFirewallRule -DisplayName "p2p-bridge mDNS"  -Direction Inbound -Protocol UDP -LocalPort 5353 -Action Allow
New-NetFirewallRule -DisplayName "p2p-bridge WS"   -Direction Inbound -Protocol TCP -LocalPort 3000 -Action Allow
New-NetFirewallRule -DisplayName "p2p-bridge P2P"  -Direction Inbound -Protocol TCP -LocalPort 5050 -Action Allow
```

Linux: `sudo ufw allow 5353/udp && sudo ufw allow 3000/tcp && sudo ufw allow 5050/tcp`

## Platform notes

- **Windows** — build on Windows (`cargo build --release`) or cross-compile
  `x86_64-pc-windows-gnu`. mdns-sd is pure Rust, no Bonjour service needed.
- **Linux / macOS** — works out of the box; make sure avahi is not already
  squatting multicast (usually fine, both set `SO_REUSE*`).
- **Android** — cross-compile with [`cargo-ndk`](https://github.com/bbqsrc/cargo-ndk):
  `cargo ndk -t arm64-v8a -o app/libs build` and load the library from a thin
  Kotlin shell, or implement the companion in Kotlin:
  `NsdManager.registerService("_p2pbridge", PROTOCOL_DNS_SD, 5050)` with a TXT
  record `platform=android`, then speak the JSON handshake above on a plain
  `ServerSocket`. The protocol is intentionally tiny (one file, ~150 lines).
- **Docker** — mDNS needs host networking and the loopback flag:
  `--network host` + `--mdns-loopback` for same-host testing.

## Troubleshooting

| symptom | cause / fix |
| --- | --- |
| No devices show up | AP isolation blocks multicast (common on guest Wi-Fi); corporate Wi-Fi often filters 224.0.0.251 — try a phone hotspot |
| Device stays after shutdown | Should disappear instantly (goodbye); after 45 s the TTL reaper removes it regardless |
| Connect times out | Peer firewall blocks TCP 5050, or device went offline between discovery and connect |
| File sha256 mismatch warning on plain http | `crypto.subtle` is only available on secure origins; the browser skips hashing, the peer accepts unverified (LAN trust) |
| Two processes on one machine can't see each other | add `--mdns-loopback` |

## Project layout

```
p2p-bridge/
├── build.bat               # release build + package the Single app folder
├── build.rs                # embeds static/logo.ico into the Windows exes
├── Cargo.toml
├── static/                  # embedded multi-page dashboard (LocalSend-style)
│   ├── index.html           # app shell: sidebar + Home/Devices/Chats/Activity/Settings
│   ├── logo.svg             # app logo (favicon + header + Home hero), accent-colored
│   ├── logo.ico             # Windows exe/taskbar icon
│   ├── style.css            # design system, dark + light themes
│   └── app.js               # vanilla-JS page router + WebSocket client
└── src/
    ├── main.rs              # p2p-bridge-server: wiring & startup
    ├── lib.rs
    ├── access.rs            # inbound policy: Public / Private / Protected + password
    ├── browser.rs           # auto-open the dashboard as an app window
    ├── config.rs            # service constants, CLI, platform/IP detection
    ├── mdns.rs              # mDNS announce / reannounce / goodbye (mdns-sd)
    ├── registry.rs          # live device table: browse loop + TTL reaper
    ├── p2p.rs               # TCP handshake, sessions, chat, RTT, file transfer
    ├── files.rs             # inbound file assembly, sanitise, SHA-256, save
    ├── protocol.rs          # all JSON wire schemas (WS + P2P)
    ├── server.rs            # Axum router, WS control channel, static assets
    └── bin/device_simulator.rs  # 5 fake peers incl. flaky Wi-Fi one
```
