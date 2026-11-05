package app.single.bridge;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.webkit.WebView;
import android.webkit.WebViewClient;

/**
 * Minimal wrapper: starts {@link ServerService} (which spawns the Rust
 * server bundled as libsingle.so) and shows the dashboard at
 * http://127.0.0.1:3000 in a WebView. No AndroidX, no extra dependencies —
 * the whole UI is the same multi-page dashboard as on desktop.
 */
public class MainActivity extends Activity {

    private static final String DASH_URL = "http://127.0.0.1:3000";
    private static final long RETRY_MS = 700;

    private WebView web;
    private final Handler ui = new Handler();
    private boolean dashboardUp = false;

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

        web.setWebViewClient(new WebViewClient() {
            @Override
            public void onPageFinished(WebView view, String url) {
                dashboardUp = true;
            }

            @Override
            public void onReceivedError(WebView view, int code, String desc, String url) {
                // Server still booting (first launch) — retry until it answers.
                if (!dashboardUp && url != null && url.startsWith("http")) {
                    ui.postDelayed(() -> web.loadUrl(DASH_URL), RETRY_MS);
                }
            }
        });

        // Foreground service owns the server process (and the mDNS lock).
        startService(new Intent(this, ServerService.class));

        web.loadUrl(DASH_URL);
    }

    @Override
    protected void onStart() {
        super.onStart();
        // Coming back after the OS freed memory: reload if the page died.
        if (dashboardUp && web.getUrl() == null) web.loadUrl(DASH_URL);
    }

    @Override
    public void onBackPressed() {
        if (web.canGoBack()) web.goBack();
        else super.onBackPressed();
    }

    @Override
    protected void onDestroy() {
        // Swipe the app away = stop the server (the service kills the process
        // explicitly and releases the multicast lock).
        stopService(new Intent(this, ServerService.class));
        super.onDestroy();
    }
}
