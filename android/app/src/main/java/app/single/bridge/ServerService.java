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

/**
 * Runs the Rust server binary (packaged into the APK as
 * jniLibs/&lt;abi&gt;/libsingle.so, extracted by Android to nativeLibraryDir)
 * as a foreground service and keeps a Wi-Fi MulticastLock held so mDNS
 * discovery works. The process is killed when the service stops.
 */
public class ServerService extends Service {

    private static final String TAG = "Single";
    private static final String CHANNEL_ID = "single-server";
    private static final int NOTIFY_ID = 1;

    private Process server;
    private WifiManager.MulticastLock multicastLock;

    @Override
    public void onCreate() {
        super.onCreate();
        startForegroundWithNotice("starting…");
        holdMulticastLock();
        startServerProcess();
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId) {
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
            server = pb.start();
            Log.i(TAG, "server started from " + bin.getAbsolutePath()
                    + "  · files land in " + recv.getAbsolutePath());

            // Pump server stdout/stderr into logcat so `adb logcat -s Single`
            // shows the same activity feed the desktop console prints.
            Thread pump = new Thread(() -> {
                try (BufferedReader r = new BufferedReader(
                        new InputStreamReader(server.getInputStream()))) {
                    String line;
                    while ((line = r.readLine()) != null) Log.i(TAG, line);
                } catch (Exception ignored) {
                }
            }, "single-log-pump");
            pump.setDaemon(true);
            pump.start();
        } catch (Exception e) {
            Log.e(TAG, "cannot start the server binary", e);
        }
    }

    private void stopServerProcess() {
        if (server != null) {
            server.destroy(); // SIGTERM → graceful shutdown + mDNS goodbye
            try {
                server.waitFor();
            } catch (InterruptedException ignored) {
                Thread.currentThread().interrupt();
            }
            server = null;
            Log.i(TAG, "server stopped");
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
