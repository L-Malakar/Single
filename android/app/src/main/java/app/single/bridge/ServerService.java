package app.single.bridge;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.SystemClock;
import android.util.Log;

import java.io.BufferedReader;
import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.util.concurrent.TimeUnit;

/**
 * Runs the Rust server binary (packaged into the APK as
 * jniLibs/&lt;abi&gt;/libsingle.so, extracted by Android to nativeLibraryDir)
 * as a foreground service and keeps a Wi-Fi MulticastLock held so mDNS
 * discovery works.
 *
 * v0.1.3 hardening (fixes the reopen → ERR_CONNECTION_REFUSED bug):
 *  • START_NOT_STICKY — the activity owns this service's lifecycle. Sticky
 *    restarts used to resurrect the server headless from the background
 *    (crashing on Android 12+ ForegroundServiceStartNotAllowedException and
 *    leaving duplicate servers fighting over port 3000).
 *  • the dashboard UI ships as assets/web/* and is synced to filesDir/web
 *    on every start; the server serves it via --static-dir. UI updates no
 *    longer need to rebuild the Rust core, and if the copy ever fails the
 *    server transparently falls back to its embedded copy.
 *  • a watcher thread restarts the server process if it exits early — the
 *    classic cause was a quick app reopen racing the old instance's port
 *    teardown (bind → "address already in use" → exit → nothing listens).
 *    Retries back off and give up after repeated failures.
 *  • stop kills the process for real: SIGTERM, wait 2 s, then
 *    destroyForcibly() so the port is always free for the next launch.
 */
public class ServerService extends Service {

    private static final String TAG = "Single";
    private static final String CHANNEL_ID = "single-server";
    private static final int NOTIFY_ID = 1;

    /** Files that make up the web dashboard (bundled as APK assets). */
    private static final String[] WEB_FILES =
            {"index.html", "style.css", "app.js", "logo.svg"};

    /** A fresh process dying within this window counts as a failed boot. */
    private static final long BOOT_GRACE_MS = 15_000;
    /** Longest pause between restart attempts. */
    private static final long RETRY_CAP_MS = 5_000;
    /** Give up after this many consecutive failed boots. */
    private static final int MAX_BOOT_TRIES = 12;

    private Process server;
    private WifiManager.MulticastLock multicastLock;
    private volatile boolean stopping = false;
    private int bootTries = 0;

    @Override
    public void onCreate() {
        super.onCreate();
        startForegroundWithNotice("starting…");
        holdMulticastLock();
        startServerProcess();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // NOT sticky: after the app is swiped away the system must NOT
        // resurrect us in the background — MainActivity starts us on every
        // launch, and a zombie service would collide with the next one on
        // port 3000 (or crash on Android 12+ background-start rules).
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy() {
        stopping = true;
        stopServerProcess();
        releaseMulticastLock();
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent) {
        return null; // not bindable
    }

    // ------------------------------------------------------------------ bits

    private void holdMulticastLock() {
        try {
            WifiManager wifi =
                    (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wifi != null) {
                multicastLock = wifi.createMulticastLock("single-mdns");
                multicastLock.setReferenceCounted(false);
                multicastLock.acquire();
            }
        } catch (Exception e) {
            Log.w(TAG, "multicast lock unavailable", e);
        }
    }

    private void releaseMulticastLock() {
        try {
            if (multicastLock != null && multicastLock.isHeld()) multicastLock.release();
        } catch (Exception ignored) {
        }
        multicastLock = null;
    }

    /** Copies assets/web/* → filesDir/web so the server can --static-dir it. */
    private File syncWebAssets() {
        File dir = new File(getFilesDir(), "web");
        if (!dir.isDirectory() && !dir.mkdirs()) return dir;
        for (String name : WEB_FILES) {
            File out = new File(dir, name);
            try (InputStream in = getAssets().open("web/" + name);
                 FileOutputStream os = new FileOutputStream(out)) {
                byte[] buf = new byte[16_384];
                int n;
                while ((n = in.read(buf)) > 0) os.write(buf, 0, n);
            } catch (IOException e) {
                Log.w(TAG, "web asset sync failed for " + name, e);
                out.delete(); // partial file → server falls back to embedded UI
            }
        }
        return dir;
    }

    private synchronized void startServerProcess() {
        killServerLocked(); // defensive: never two processes at once

        File web = syncWebAssets();
        try {
            File bin = new File(getApplicationInfo().nativeLibraryDir, "libsingle.so");
            File base = getExternalFilesDir(null);
            if (base == null) base = getFilesDir();
            File recv = new File(base, "received");
            recv.mkdirs();

            String name = Build.MODEL == null ? "android-device" : Build.MODEL.trim();

            ProcessBuilder pb = new ProcessBuilder(
                    bin.getAbsolutePath(),
                    "--no-open",
                    "--name", name,
                    "--http-port", "3000",
                    "--p2p-port", "5050",
                    "--received-dir", recv.getAbsolutePath(),
                    "--static-dir", web.getAbsolutePath());
            pb.redirectErrorStream(true);
            Process proc = pb.start();
            server = proc;
            bootTries++;
            final long startedAt = SystemClock.elapsedRealtime();
            Log.i(TAG, "server started from " + bin.getAbsolutePath()
                    + " · ui from " + web.getAbsolutePath()
                    + " · files land in " + recv.getAbsolutePath());
            setNoticeText("running");

            // Pump server stdout/stderr into logcat so `adb logcat -s Single`
            // shows the same activity feed the desktop console prints.
            Thread pump = new Thread(() -> {
                try (BufferedReader r = new BufferedReader(
                        new InputStreamReader(proc.getInputStream()))) {
                    String line;
                    while ((line = r.readLine()) != null) Log.i(TAG, line);
                } catch (Exception ignored) {
                }
            }, "single-log-pump");
            pump.setDaemon(true);
            pump.start();

            // Watcher: an early exit (port still held by the previous
            // instance, slow teardown…) is retried with backoff. A process
            // that ran long and then died is restarted once from scratch.
            Thread watch = new Thread(() -> {
                int code;
                try {
                    code = proc.waitFor();
                } catch (InterruptedException e) {
                    return;
                }
                if (stopping) return;
                long uptime = SystemClock.elapsedRealtime() - startedAt;
                if (uptime >= BOOT_GRACE_MS) bootTries = 0; // it was healthy
                if (bootTries > MAX_BOOT_TRIES) {
                    Log.e(TAG, "server keeps failing (last exit " + code + ") — giving up");
                    setNoticeText("stopped — reopen the app");
                    return;
                }
                long backoff = Math.min(400L << Math.min(bootTries, 4), RETRY_CAP_MS);
                Log.w(TAG, "server exited early (code " + code + ", uptime " + uptime
                        + " ms) — restarting in " + backoff + " ms");
                setNoticeText("restarting…");
                try {
                    Thread.sleep(backoff);
                } catch (InterruptedException e) {
                    return;
                }
                if (!stopping) startServerProcess();
            }, "single-server-watch");
            watch.setDaemon(true);
            watch.start();
        } catch (Exception e) {
            Log.e(TAG, "cannot start the server binary", e);
            setNoticeText("failed to start — reopen the app");
        }
    }

    /** SIGTERM → graceful shutdown; escalate if it refuses to die. */
    private void stopServerProcess() {
        synchronized (this) {
            killServerLocked();
        }
        Log.i(TAG, "server stopped");
    }

    private void killServerLocked() {
        Process p = server;
        server = null;
        if (p == null) return;
        p.destroy(); // SIGTERM → graceful shutdown + mDNS goodbye
        try {
            if (!p.waitFor(2, TimeUnit.SECONDS)) {
                if (Build.VERSION.SDK_INT >= 26) {
                    Log.w(TAG, "server ignored SIGTERM — killing forcibly");
                    p.destroyForcibly();
                    p.waitFor(2, TimeUnit.SECONDS);
                }
            }
        } catch (InterruptedException ignored) {
            Thread.currentThread().interrupt();
        }
    }

    private void startForegroundWithNotice(String text) {
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (Build.VERSION.SDK_INT >= 26) {
            NotificationChannel ch = new NotificationChannel(
                    CHANNEL_ID, "Single server", NotificationManager.IMPORTANCE_LOW);
            ch.setDescription("Keeps the LAN server alive");
            nm.createNotificationChannel(ch);
        }
        Notification n;
        if (Build.VERSION.SDK_INT >= 26) {
            n = new Notification.Builder(this, CHANNEL_ID)
                    .setContentTitle("Single is running")
                    .setContentText(text + " — this device is visible on your Wi-Fi.")
                    .setSmallIcon(R.drawable.ic_stat_notify)
                    .setOngoing(true)
                    .setOnlyAlertOnce(true)
                    .build();
        } else {
            n = new Notification.Builder(this)
                    .setContentTitle("Single is running")
                    .setContentText(text + " — this device is visible on your Wi-Fi.")
                    .setSmallIcon(R.drawable.ic_stat_notify)
                    .setPriority(Notification.PRIORITY_LOW)
                    .setOngoing(true)
                    .build();
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFY_ID, n,
                    android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(NOTIFY_ID, n);
        }
    }

    private void setNoticeText(String text) {
        NotificationManager nm = (NotificationManager) getSystemService(NOTIFICATION_SERVICE);
        if (nm == null) return;
        Notification n;
        if (Build.VERSION.SDK_INT >= 26) {
            n = new Notification.Builder(this, CHANNEL_ID)
                    .setContentTitle("Single is running")
                    .setContentText(text + " — this device is visible on your Wi-Fi.")
                    .setSmallIcon(R.drawable.ic_stat_notify)
                    .setOngoing(true)
                    .setOnlyAlertOnce(true)
                    .build();
        } else {
            n = new Notification.Builder(this)
                    .setContentTitle("Single is running")
                    .setContentText(text + " — this device is visible on your Wi-Fi.")
                    .setSmallIcon(R.drawable.ic_stat_notify)
                    .setPriority(Notification.PRIORITY_LOW)
                    .build();
        }
        try {
            nm.notify(NOTIFY_ID, n);
        } catch (SecurityException ignored) {
            // notification permission not granted — the service keeps running
        }
    }
}
