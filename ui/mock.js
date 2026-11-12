/* Single — browser preview mock.
 *
 * When app.js runs inside the real Tauri window, window.__TAURI__ exists and
 * this file does nothing. When you open ui/index.html in a plain browser
 * (e.g. to preview the design), this mock stands in for the Rust backend so
 * every screen renders with sample data.
 */
(function () {
  if (window.__TAURI__) return; // real app — keep out of the way

  const devices = [
    { id: "a1b2c3d4e5f60718", name: "Living-Room-PC", addr: "192.168.1.23", port: 27500, paired: true, online: true, drive_letter: "S", source: "mdns", is_self: false },
    { id: "f1e2d3c4b5a69788", name: "Redmi-Note", addr: "192.168.1.34", port: 27500, paired: false, online: true, drive_letter: null, source: "beacon", is_self: false },
  ];

  const fs = {
    "": [
      { name: "Movies", path: "Movies", is_dir: true, size: 0, mtime: 1731000000 },
      { name: "Photos", path: "Photos", is_dir: true, size: 0, mtime: 1730900000 },
      { name: "project-report.pdf", path: "project-report.pdf", is_dir: false, size: 2_412_344, mtime: 1730981234 },
      { name: "family-video.mp4", path: "family-video.mp4", is_dir: false, size: 734_003_200, mtime: 1730800000 },
    ],
    "Movies": [
      { name: "holiday-2025.mp4", path: "Movies/holiday-2025.mp4", is_dir: false, size: 1_904_332_800, mtime: 1730700000 },
      { name: "desk-setup.mkv", path: "Movies/desk-setup.mkv", is_dir: false, size: 402_000_000, mtime: 1730600000 },
    ],
    "Photos": [
      { name: "IMG_2041.jpg", path: "Photos/IMG_2041.jpg", is_dir: false, size: 3_204_112, mtime: 1730500000 },
      { name: "IMG_2042.jpg", path: "Photos/IMG_2042.jpg", is_dir: false, size: 2_994_002, mtime: 1730500100 },
    ],
  };

  let transfers = [
    { id: 1, kind: "download", peer_id: "a1b2c3d4e5f60718", peer_name: "Living-Room-PC", name: "family-video.mp4", path: "family-video.mp4", done: 512_000_000, total: 734_003_200, state: "active" },
    { id: 2, kind: "upload", peer_id: "a1b2c3d4e5f60718", peer_name: "Living-Room-PC", name: "notes.txt", path: "notes.txt", done: 1024, total: 1024, state: "done" },
  ];

  const handlers = {};

  window.__TAURI__ = {
    core: {
      async invoke(cmd, args = {}) {
        await new Promise((r) => setTimeout(r, 120));
        switch (cmd) {
          case "get_app_info":
            return { id: "0a1b2c3d4e5f6789", name: "My-Laptop", ca_fp: "ff".repeat(32), ips: ["192.168.1.10"], port: 27500, share_dir: "C:\\Users\\me\\.lmalakar\\Single\\Share", version: "0.1.2", platform: "windows", theme: "dark", auto_map_drives: true, root_mode: false };
          case "list_devices": return devices;
          case "list_transfers": return transfers;
          case "browse": {
            const path = args.path ?? "";
            if (args.peer) return fs[path] ?? [];
            return fs[path] ?? [];
          }
          case "pair_start": return "mock-req-id-1";
          case "pair_status": return "pending";
          case "pair_incoming": return [];
          case "win_list_drives": return [["S", "https://192.168.1.23:27500/dav"]];
          case "download_file":
          case "upload_files":
            return 99;
          case "set_settings": return window.__TAURI__.core.invoke("get_app_info");
          default: return null;
        }
      },
    },
    event: {
      listen(ev, cb) {
        (handlers[ev] ??= []).push(cb);
        if (ev === "pair-incoming") {
          setTimeout(() => cb({
            payload: { type: "pair_incoming", req: { req_id: "mock1", id: "f1e2d3c4b5a69788", name: "Redmi-Note", pin: "483921", state: "pending", pin_ok: false } },
          }), 1500);
        }
        return Promise.resolve(() => {});
      },
      _emit(ev, payload) { (handlers[ev] ?? []).forEach((cb) => cb({ payload })); },
    },
    dialog: {
      async open() { return "C:\\Users\\me\\Documents\\demo.txt"; },
    },
    opener: {
      openUrl(u) { window.open(u, "_blank"); },
      openPath() {},
    },
  };

  console.log("[Single] browser preview mock active — sample data only");
})();
