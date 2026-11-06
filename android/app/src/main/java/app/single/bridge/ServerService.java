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
import android.util.Log;

import java.io.BufferedReader;
import java.io.File;
import java.io.InputStreamReader;
import java.util.concurrent.TimeUnit;

/**
 * Runs the Rust server binary (packaged into the APK as
 * jniLibs/&lt;abi&gt;/libsingle.so, extracted by Android to nativeLibraryDir)
 * as a foreground service and keeps a Wi-Fi MulticastLock held so mDNS
 * discovery works. The process is killed when the service stops.
 *
 * A watchdog restarts the server process if it exits unexpectedly (e.g. a
 * port still held by a dying predecessor right after the app was swiped
 * away) — up to three quick attempts, so reopening the app always works.
 */
public class ServerService extends Service {

    private static final String TAG = "Single";
    private static final String CHANNEL_ID = "single-server";
    private static final int NOTIFY_ID = 1;
    private static final int MAX_RESTARTS = 3;

    private final Object procLock = new Object();
    private Process server;
    private int restarts = 0;
    private boolean stopping = false;
    private WifiManager.MulticastLock multicastLock;

    @Override
    public void onCreate() {
        super.onCreate();
        stopping = false;
        restarts = 0;
        startForegroundWithNotice("starting…");
        holdMulticastLock();
        startServerProcess();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
        // Started again while already running (app reopened): make sure a
        // previously failed server process gets another chance.
        synchronized (procLock) {
            if (server == null && !stopping) startServerProcess();
        }
        return START_STICKY;
    }

    @Override
    public void onDestroy() {
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

    private void startServerProcess() {
        synchronized (procLock) {
            if (stopping || server != null) return;
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
                        "--received-dir", recv.getAbsolutePath());
                pb.redirectErrorStream(true);
                pb.directory(getFilesDir()); // writable cwd; state file lands next to received/
                final Process p = pb.start();
                server = p;
                startForegroundWithNotice("running");
                Log.i(TAG, "server started from " + bin.getAbsolutePath()
                        + "  · files land in " + recv.getAbsolutePath());

                // Pump server stdout/stderr into logcat so `adb logcat -s Single`
                // shows the same activity feed the desktop console prints.
                Thread pump = new Thread(() -> {
                    try (BufferedReader r = new BufferedReader(
                            new InputStreamReader(p.getInputStream()))) {
                        String line;
                        while ((line = r.readLine()) != null) Log.i(TAG, line);
                    } catch (Exception ignored) {
                    }
                }, "single-log-pump");
                pump.setDaemon(true);
                pump.start();

                // Watchdog: if the process dies on its own (crash, port
                // conflict with a not-yet-dead predecessor), restart it.
                Thread watch = new Thread(() -> {
                    int code = -1;
                    try { code = p.waitFor(); } catch (InterruptedException ignored) { }
                    synchronized (procLock) {
                        if (server == p && !stopping) {
                            server = null;
                            if (restarts < MAX_RESTARTS) {
                                restarts++;
                                Log.w(TAG, "server exited (code " + code
                                        + ") — restarting, attempt " + restarts);
                                try { Thread.sleep(700); } catch (InterruptedException e) {
                                    return;
                                }
                                startServerProcess();
                            } else {
                                Log.e(TAG, "server keeps exiting — giving up");
                                startForegroundWithNotice("stopped — reopen the app to retry");
                            }
                        }
                    }
                }, "single-watchdog");
                watch.setDaemon(true);
                watch.start();
            } catch (Exception e) {
                Log.e(TAG, "cannot start the server binary", e);
                startForegroundWithNotice("failed to start — reopen the app to retry");
            }
        }
    }

    private void stopServerProcess() {
        synchronized (procLock) {
            stopping = true;
            Process p = server;
            server = null;
            if (p != null) {
                p.destroy(); // SIGTERM → graceful shutdown + mDNS goodbye + state flush
                try {
                    // Bounded wait: never hang the main thread on a stubborn child.
                    if (!p.waitFor(2, TimeUnit.SECONDS)) {
                        p.destroyForcibly();
                        p.waitFor(1, TimeUnit.SECONDS);
                    }
                } catch (InterruptedException e) {
                    Thread.currentThread().interrupt();
                }
                Log.i(TAG, "server stopped");
            }
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
                    .build();
        } else {
            n = new Notification.Builder(this)
                    .setContentTitle("Single is running")
                    .setContentText(text + " — this device is visible on your Wi-Fi.")
                    .setSmallIcon(R.drawable.ic_stat_notify)
                    .setPriority(Notification.PRIORITY_LOW)
                    .build();
        }
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFY_ID, n,
                    android.content.pm.ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(NOTIFY_ID, n);
        }
    }
}
