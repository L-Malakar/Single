#!/usr/bin/env python3
"""Screenshot the redesigned dashboard: Home page, sidebar collapse, Protocol UI."""
import subprocess, sys, time, urllib.request
from pathlib import Path
from playwright.sync_api import sync_playwright

ROOT = Path(__file__).resolve().parent.parent
SERVER = ROOT / "target" / "debug" / "p2p-bridge-server"
STATIC = ROOT / "static"
OUT = ROOT / "shots"

HTTP = 3931
BASE = f"http://127.0.0.1:{HTTP}"

server = subprocess.Popen(
    [str(SERVER),
     "--name", "Studio-PC", "--http-port", str(HTTP), "--p2p-port", "5952",
     "--no-announce", "--no-open", "--static-dir", str(STATIC),
     "--received-dir", "/tmp/shot-received"],
    stdout=subprocess.DEVNULL, stderr=subprocess.STDOUT)
try:
    for _ in range(60):
        try:
            urllib.request.urlopen(f"{BASE}/api/health", timeout=1)
            break
        except Exception:
            time.sleep(0.25)
    else:
        sys.exit("server never came up")

    with sync_playwright() as p:
        browser = p.chromium.launch()
        pg = browser.new_page(viewport={"width": 1280, "height": 800})
        errors = []
        pg.on("pageerror", lambda e: errors.append(str(e)))
        pg.on("console", lambda m: errors.append(m.text) if m.type == "error" else None)

        pg.goto(BASE, wait_until="networkidle")
        pg.wait_for_timeout(900)

        # 1. Home, sidebar expanded
        assert pg.locator("#page-home.active").count() == 1, "home page not active on boot"
        name = pg.text_content("#homeName")
        assert name and name.strip(), "home device name empty"
        pg.screenshot(path=f"{OUT}/1-home-expanded.png")

        # 2. Protocol: switch to Protected -> password row appears
        pg.click('#protocolSeg .chip[data-m="protected"]')
        pg.wait_for_timeout(300)
        assert pg.locator("#protoPassRow:not(.hidden)").count() == 1, "password row hidden"
        pg.fill("#protoPass", "lan-key-42")
        pg.click("#protoPassSave")
        pg.wait_for_timeout(400)
        pg.screenshot(path=f"{OUT}/2-home-protected.png")

        # 3. Collapse the sidebar (YouTube-style)
        pg.click("#sideToggle")
        pg.wait_for_timeout(500)
        assert pg.evaluate("document.body.classList.contains('side-mini')"), "body lacks side-mini"
        label_hidden = pg.evaluate(
            "getComputedStyle(document.querySelector('#sideNav .nav-item .nav-label')).display === 'none'")
        assert label_hidden, "nav labels still visible when collapsed"
        pg.screenshot(path=f"{OUT}/3-home-collapsed.png")

        # 4. Find button -> Devices page (still collapsed)
        pg.click("#findBtn")
        pg.wait_for_timeout(500)
        assert pg.locator("#page-devices.active").count() == 1, "Find did not navigate to Devices"
        pg.screenshot(path=f"{OUT}/4-devices-collapsed.png")

        # 5. Back Home + tap the device-name pill -> Settings (rename)
        pg.click('#sideNav .nav-item[data-page="home"]')
        pg.wait_for_timeout(300)
        pg.click("#homeNameBtn")
        pg.wait_for_timeout(300)
        assert pg.locator("#page-settings.active").count() == 1, "name pill did not open Settings"
        pg.screenshot(path=f"{OUT}/5-settings.png")

        # 6. light theme home (expanded again for the record)
        pg.click('#themeSeg .chip[data-t="light"]')   # we are on Settings here
        pg.click('#sideNav .nav-item[data-page="home"]')
        pg.click("#sideToggle")           # expand
        pg.wait_for_timeout(600)
        pg.screenshot(path=f"{OUT}/6-home-light.png")

        browser.close()
        if errors:
            print("JS console errors:", *errors, sep="\n  ")
            sys.exit(1)
        print("all UI checks passed; name shown on home =", name.strip())
finally:
    server.terminate()
