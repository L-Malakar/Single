package app.single.bridge;

import android.app.Activity;
import android.content.Intent;
import android.content.res.Configuration;
import android.graphics.Color;
import android.graphics.Typeface;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.WebResourceRequest;
import android.webkit.WebSettings;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.ImageView;
import android.widget.LinearLayout;
import android.widget.TextView;

import java.net.HttpURLConnection;
import java.net.URL;

/**
 * Minimal wrapper: starts {@link ServerService} (which spawns the Rust
 * server bundled as libsingle.so) and shows the dashboard at
 * http://127.0.0.1:3000 in a WebView. No AndroidX, no extra dependencies —
 * the whole UI is the same multi-page dashboard as on desktop (phones get
 * the app bar + bottom tab bar layout automatically).
 *
 * Robust startup (the old "web page not found after reopening" bug):
 * the dashboard is loaded only AFTER the embedded server actually answers
 * a probe on 127.0.0.1:3000, and any main-frame error re-triggers that
 * probe loop — the built-in WebView error page never stays on screen.
 * A native splash (logo + themed white/black background, following the
 * system dark mode) covers the wait.
 */
public class MainActivity extends Activity {

    private static final String DASH_URL = "http://127.0.0.1:3000";

    private WebView web;
    private FrameLayout root;
    private LinearLayout splashView;
    private TextView splashStatus;
    private final Handler ui = new Handler();

    /** a poll-and-load cycle is already running (one at a time) */
    private volatile boolean polling = false;
    /** the current/last navigation ended on the built-in error page */
    private volatile boolean navFailed = false;
    /** activity is gone for good (not just recreated for a config change) */
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
        WebSettings s = web.getSettings();
        s.setJavaScriptEnabled(true);
        s.setDomStorageEnabled(true);        // theme + layout + drafts survive restarts
        s.setUseWideViewPort(true);          // fit the responsive dashboard exactly
        s.setLoadWithOverviewMode(true);     // to the screen width (pinch-zoom still works)
        web.setBackgroundColor(themeBackground());

        web.setWebViewClient(new WebViewClient() {

            @Override
            public void onPageStarted(WebView view, String url, android.graphics.Bitmap favicon) {
                // a fresh navigation is under way — arm the error detection
                if (url != null && url.startsWith("http://127.0.0.1")) navFailed = false;
            }

            @SuppressWarnings("deprecation")
            @Override
            public void onReceivedError(WebView view, int code, String desc, String url) {
                // legacy variant (still called on every WebView level) —
                // react only to a failure of the page itself, not sub-resources
                if (url != null && url.startsWith("http://127.0.0.1")) handleLoadFailure();
            }

            @Override
            public void onReceivedError(WebView view, WebResourceRequest req,
                                        android.webkit.WebResourceError err) {
                if (req.isForMainFrame()
                        && req.getUrl().toString().startsWith("http://127.0.0.1")) {
                    handleLoadFailure();
                }
            }

            @Override
            public void onPageFinished(WebView view, String url) {
                // IMPORTANT: the built-in "web page not found" page ALSO fires
                // onPageFinished. Treating that as success was the bug that
                // froze the app on the error screen after a reopen.
                if (url == null || !url.startsWith("http://127.0.0.1")) return;
                if (navFailed) {
                    handleLoadFailure();
                } else {
                    hideSplash();
                }
            }
        });

        root = new FrameLayout(this);
        root.addView(web, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        buildSplash();
        root.addView(splashView, new FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        setContentView(root);

        // The foreground service owns the server process (and the mDNS lock).
        startService(new Intent(this, ServerService.class));

        loadWhenServerReady();
    }

    private void handleLoadFailure() {
        navFailed = true;
        showSplash("restarting the server…");
        loadWhenServerReady();
    }

    /**
     * Poll the local server on a worker thread; only once it actually answers
     * do we point the WebView at it. Kills the classic race where the WebView
     * loads faster than the server binds its port (first launch AND every
     * reopen after the service was stopped).
     */
    private void loadWhenServerReady() {
        if (polling || gone || isFinishing()) return;
        polling = true;
        Thread t = new Thread(() -> {
            long delay = 200;
            while (!gone && !isFinishing()) {
                if (serverResponds()) {
                    ui.post(() -> {
                        if (!gone && !isFinishing()) web.loadUrl(DASH_URL);
                    });
                    polling = false;
                    return;
                }
                try { Thread.sleep(delay); } catch (InterruptedException e) { break; }
                delay = Math.min(delay + 150, 1500);
            }
            polling = false;
        }, "single-dash-wait");
        t.setDaemon(true);
        t.start();
    }

    /** Any HTTP answer (even 404) proves the server is up and accepting. */
    private boolean serverResponds() {
        HttpURLConnection c = null;
        try {
            c = (HttpURLConnection) new URL(DASH_URL + "/api/health").openConnection();
            c.setConnectTimeout(400);
            c.setReadTimeout(400);
            c.setRequestMethod("GET");
            int code = c.getResponseCode();
            return code >= 200 && code < 500;
        } catch (Exception e) {
            return false;
        } finally {
            if (c != null) c.disconnect();
        }
    }

    /* ------------------------------------------------------------- splash */

    private void buildSplash() {
        splashView = new LinearLayout(this);
        splashView.setOrientation(LinearLayout.VERTICAL);
        splashView.setGravity(Gravity.CENTER);
        splashView.setBackgroundColor(themeBackground());

        ImageView logo = new ImageView(this);
        logo.setImageResource(R.mipmap.ic_launcher);
        splashView.addView(logo, new LinearLayout.LayoutParams(dp(104), dp(104)));

        TextView name = new TextView(this);
        name.setText("Single");
        name.setTextSize(26);
        name.setTypeface(Typeface.DEFAULT_BOLD);
        name.setTextColor(themeText());
        name.setPadding(0, dp(16), 0, 0);
        splashView.addView(name);

        splashStatus = new TextView(this);
        splashStatus.setText("starting the server…");
        splashStatus.setTextSize(13.5f);
        splashStatus.setTextColor(themeSub());
        splashStatus.setPadding(0, dp(8), 0, 0);
        splashView.addView(splashStatus);
    }

    private void showSplash(String status) {
        if (gone || splashView == null) return;
        if (splashStatus != null) splashStatus.setText(status);
        if (splashView.getParent() == null) {
            splashView.setAlpha(1f);
            root.addView(splashView, new FrameLayout.LayoutParams(
                    ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        }
    }

    private void hideSplash() {
        if (splashView == null || splashView.getParent() == null) return;
        final View s = splashView;
        s.animate().alpha(0f).setDuration(280).withEndAction(() -> {
            if (s.getParent() != null) root.removeView(s);
        }).start();
    }

    /* ------------------------------------------------------------- theme */

    /** Match the splash/HTML background so the launch never flashes white. */
    private int themeBackground() {
        return Color.parseColor(isNight() ? "#0b0d10" : "#eef1f6");
    }

    private int themeText() {
        return Color.parseColor(isNight() ? "#f0f3f7" : "#171b21");
    }

    private int themeSub() {
        return Color.parseColor(isNight() ? "#9aa3b2" : "#5a6572");
    }

    private boolean isNight() {
        int night = getResources().getConfiguration().uiMode
                & Configuration.UI_MODE_NIGHT_MASK;
        return night == Configuration.UI_MODE_NIGHT_YES;
    }

    /** Keep the system bars in sync when the system theme flips at runtime. */
    private void applySystemBars() {
        int bg = themeBackground();
        getWindow().setStatusBarColor(bg);
        getWindow().setNavigationBarColor(bg);
        if (Build.VERSION.SDK_INT >= 23) {
            View deco = getWindow().getDecorView();
            int vis = deco.getSystemUiVisibility();
            if (isNight()) vis &= ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            else vis |= View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR;
            if (Build.VERSION.SDK_INT >= 26) {
                if (isNight()) vis &= ~View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
                else vis |= View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
            }
            deco.setSystemUiVisibility(vis);
        }
    }

    private int dp(int v) {
        return Math.round(v * getResources().getDisplayMetrics().density);
    }

    /* -------------------------------------------------------- lifecycle */

    @Override
    protected void onStart() {
        super.onStart();
        // Process was killed in the background and restored: make sure the
        // dashboard is (re)loaded. No-op when already showing.
        if (web.getUrl() == null) loadWhenServerReady();
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        // uiMode is declared in android:configChanges, so the activity is NOT
        // recreated when the system dark mode flips — recolor instead (the
        // page itself follows prefers-color-scheme live).
        applySystemBars();
        web.setBackgroundColor(themeBackground());
        if (splashView != null && splashView.getParent() != null) {
            splashView.setBackgroundColor(themeBackground());
        }
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }

    @Override
    protected void onDestroy() {
        gone = true;
        // Stop the server only when the activity is truly finished (back
        // press, swipe-away). A configuration-change recreation has
        // isFinishing() == false and must keep the service alive — killing
        // it there was another path to the "page not found" state.
        if (isFinishing()) stopService(new Intent(this, ServerService.class));
        super.onDestroy();
    }
}
