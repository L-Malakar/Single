//! p2p-bridge — a local Wi-Fi cross-platform control server.
//!
//! Architecture:
//! - [`mdns`]: broadcasts this host on the LAN as `_p2pbridge._tcp.local.`
//!   (mDNS / Bonjour / Zeroconf) and browses for peer devices.
//! - [`registry`]: live device table built from mDNS events, with TTL reaping.
//! - [`p2p`]: peer-to-peer TCP channel with a SYN/SYN-ACK style application
//!   handshake, chat, latency ping and chunked file transfer.
//! - [`server`]: Axum HTTP + WebSocket server that serves an embedded
//!   single-page dashboard and pushes live updates to every browser.

pub mod browser;
pub mod config;
pub mod files;
pub mod mdns;
pub mod p2p;
pub mod protocol;
pub mod registry;
pub mod server;
pub mod util;
