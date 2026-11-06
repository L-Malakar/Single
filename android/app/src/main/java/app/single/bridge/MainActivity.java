package app.single.bridge;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.webkit.WebResourceError;
import android.webkit.WebResourceRequest;
import android.webkit.WebView;
import android.webkit.WebViewClient;

import java.net.InetSocketAddress;
import java.net.Socket;

/**
 * Minimal wrapper: starts {@link ServerService} (which spawns the Rust
 * server bundled as libsingle.so) and shows the dashboard at
 * http://127.0.0.1:3000 in a WebView. No AndroidX, no extra dependencies —
 * the whole UI is the same multi-page dashboard as on desktop.
 *
 * ── Why the WebView never shows "net::ERR_CONNECTION_REFUSED" anymore ──
 * Older builds called loadUrl() immediately and retried on WebView errors.
 * That raced the server boot, and the retry flag was disarmed by the error
 * page's own onPageFinished — so a slow server boot left the app stuck on
 * the connection-refused page forever (classic "works on first launch,
 * broken after reopening").
 *
 * Now the flow is:
 *   1. show an inline "starting…" splash (no network involved),
 *   2. a background thread TCP-probes 127.0.0.1:3000 until the server
 *      actually accepts connections,
 *   3. only then is the dashboard loaded — and if the server ever dies
 *      mid-session, the main-frame error handler swaps back to the splash
 *      and re-enters the probe loop. The Chromium error page is never the
 *      final state the user sees.
 */
public class MainActivity extends Activity {

    private static final String DASH_URL = "http://127.0.0.1:3000";
    private static final String DASH_HOST = "127.0.0.1";
    private static final int DASH_PORT = 3000;

    private WebView web;
    private final Handler ui = new Handler();
    private volatile boolean probing = false;
    private volatile boolean gone = false;

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);

        // Notification permission is optional — only affects the visibility
        // of the "server running" notification on Android 13+.
        if (Build.VERSION.SDK_INT >= 33) {
            requestPermissions(new String[]{"android.permission.POST_NOTIFICATIONS"}, 1);
        }

        web = new WebView(this);
        web.setBackgroundColor(Color.parseColor("#0b0d10"));
        web.getSettings().setJavaScriptEnabled(true);
        web.getSettings().setDomStorageEnabled(true); // theme + sidebar preferences
        setContentView(web);

        web.setWebViewClient(new DashWebClient(this));

        // Foreground service owns the server process (and the mDNS lock).
        startService(new Intent(this, ServerService.class));

        showSplash("Starting the server…");
        startProbing();
    }

    /**
     * WebView callbacks. Named (not anonymous) inner class on purpose:
     * the d8 dexer shipped in build-tools 34 crashes on anonymous-class
     * bytecode emitted by modern JDK javacs.
     */
    private static class DashWebClient extends WebViewClient {

        private final MainActivity host;

        DashWebClient(MainActivity host) {
            this.host = host;
        }

        /** Modern main/sub-frame errors (API 23+, minSdk 24). */
        @Override
        public void onReceivedError(WebView view, WebResourceRequest req, WebResourceError err) {
            if (req.isForMainFrame()) host.serverVanished();
        }

        /** Legacy main-frame errors — kept for older WebViews. */
        @SuppressWarnings("deprecation")
        @Override
        public void onReceivedError(WebView view, int code, String desc, String url) {
            if (url != null && url.startsWith("http")) host.serverVanished();
        }

        @Override
        public void onPageFinished(WebView view, String url) {
            // The dashboard really rendered: drop the splash/error page
            // from the back history so Back exits the app instead of
            // surfacing "starting…" again.
            if (url != null && url.startsWith(DASH_URL)) view.clearHistory();
        }
    }

    @Override
    protected void onStart() {
        super.onStart();
        // Coming back after the OS freed memory: if the page died while we
        // were away, go through the probe loop again (it loads instantly
        // when the server is still alive).
        String cur = web.getUrl();
        if (cur == null || !cur.startsWith(DASH_URL)) startProbing();
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }

    @Override
    protected void onDestroy() {
        gone = true;
        // Explicit close (Back press out of the app): stop the server — the
        // service kills the process cleanly and releases the multicast lock.
        // System-initiated destroys (memory pressure while backgrounded)
        // leave the server running so transfers survive.
        if (isFinishing()) stopService(new Intent(this, ServerService.class));
        super.onDestroy();
    }

    // ------------------------------------------------------------- recovery

    /** Main frame failed to load → the server is (temporarily) gone. */
    private void serverVanished() {
        ui.post(() -> {
            if (gone) return;
            showSplash("Waiting for the server…");
            startProbing();
        });
    }

    /**
     * Polls 127.0.0.1:3000 on a worker thread (250 ms → 1 s backoff) and
     * loads the dashboard on the UI thread as soon as it answers. Exactly
     * one loop runs at any time.
     */
    private void startProbing() {
        if (probing || gone) return;
        probing = true;
        Thread t = new Thread(() -> {
            long delay = 250;
            while (!gone) {
                if (portOpen()) {
                    ui.post(() -> { if (!gone) web.loadUrl(DASH_URL); });
                    break;
                }
                try { Thread.sleep(delay); } catch (InterruptedException e) { break; }
                delay = Math.min(delay + 150, 1000);
            }
            probing = false;
        }, "single-port-probe");
        t.setDaemon(true);
        t.start();
    }

    private static boolean portOpen() {
        try (Socket s = new Socket()) {
            s.connect(new InetSocketAddress(DASH_HOST, DASH_PORT), 400);
            return true;
        } catch (Exception e) {
            return false;
        }
    }

    // -------------------------------------------------------------- splash

    /**
     * Inline, network-free splash/status page (also the "reconnecting…"
     * screen). Kept in sync with the dashboard's dark theme; the spinner
     * is a pure CSS ring. Loaded with a null base URL so it can never
     * trigger the main-frame error handler.
     */
    private void showSplash(String message) {
        String html =
            "<!doctype html><html><head><meta charset='utf-8'>" +
            "<meta name='viewport' content='width=device-width, initial-scale=1, viewport-fit=cover'>" +
            "<style>" +
            "html,body{height:100%;margin:0}" +
            "body{display:grid;place-items:center;background:#0b0d10;color:#9aa3b2;" +
            "font:14px -apple-system,'Segoe UI',Roboto,sans-serif;text-align:center}" +
            ".wrap{display:flex;flex-direction:column;align-items:center;gap:16px;" +
            "padding:0 24px calc(0px)}" +
            ".logo{width:52px;height:52px;border-radius:14px;background:linear-gradient(135deg,#0a84ff,#409cff);" +
            "display:grid;place-items:center;box-shadow:0 10px 30px rgba(10,132,255,.35)}" +
            ".logo svg{width:30px;height:30px}" +
            ".name{color:#f0f3f7;font-size:19px;font-weight:700;letter-spacing:-.3px;margin-top:2px}" +
            ".ring{width:22px;height:22px;border-radius:50%;border:2.5px solid rgba(255,255,255,.14);" +
            "border-top-color:#0a84ff;animation:spin .9s linear infinite}" +
            "@keyframes spin{to{transform:rotate(360deg)}}" +
            ".msg{font-size:13px}" +
            "</style></head><body><div class='wrap'>" +
            "<div class='logo'><svg viewBox='0 0 24 24' fill='none' stroke='#fff' stroke-width='1.9' " +
            "stroke-linecap='round' stroke-linejoin='round'><path d='M4 10.8 12 4l8 6.8'/>" +
            "<path d='M6.2 9.5V20h11.6V9.5'/></svg></div>" +
            "<div class='name'>Single</div>" +
            "<div class='ring'></div>" +
            "<div class='msg'>" + message + "</div>" +
            "</div></body></html>";
        web.loadDataWithBaseURL(null, html, "text/html", "utf-8", null);
    }
}
