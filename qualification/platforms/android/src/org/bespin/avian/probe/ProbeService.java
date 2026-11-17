package org.bespin.avian.probe;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.VpnService;
import android.os.ParcelFileDescriptor;
import android.util.Log;
import org.json.JSONObject;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;

/** Exercises platform APIs only. No engine, radio, or default-route forwarding. */
public final class ProbeService extends VpnService {
    private volatile boolean running;
    private ParcelFileDescriptor tunnel;
    private Thread worker;

    @Override public int onStartCommand(Intent intent, int flags, int identifier) {
        NotificationManager manager = getSystemService(NotificationManager.class);
        manager.createNotificationChannel(new NotificationChannel("probe", "AVIAN probe", NotificationManager.IMPORTANCE_LOW));
        startForeground(1, new Notification.Builder(this, "probe")
            .setContentTitle("AVIAN isolated platform probe")
            .setSmallIcon(android.R.drawable.stat_sys_upload).build());
        if (running) return START_NOT_STICKY;
        running = true;
        int port = intent.getIntExtra("port", 0);
        worker = new Thread(new Runnable() {
            @Override public void run() { runProbe(port); }
        }, "avian-platform-probe");
        worker.start();
        return START_NOT_STICKY;
    }

    private void runProbe(int port) {
        try {
            ConnectivityManager connectivity = getSystemService(ConnectivityManager.class);
            Network physicalNetwork = null;
            String transport = "unknown";
            for (int attempt = 0; attempt < 20 && physicalNetwork == null; attempt++) {
                Network network = connectivity.getActiveNetwork();
                NetworkCapabilities capabilities = network == null ? null : connectivity.getNetworkCapabilities(network);
                if (capabilities != null && !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
                        && capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                        && capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)) {
                    physicalNetwork = network;
                    transport = capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) ? "wifi" : "cellular";
                } else {
                    Thread.sleep(500);
                }
            }
            if (physicalNetwork == null) throw new IllegalStateException("No physical network");
            tunnel = new Builder().setSession("AVIAN isolated probe").setMtu(1280)
                .addAddress("fdab:49a9:1::2", 128).addRoute("fdab:49a9:1::", 64)
                .allowFamily(android.system.OsConstants.AF_INET)
                .setUnderlyingNetworks(new Network[] { physicalNetwork })
                .setBlocking(false).establish();
            if (tunnel == null) throw new IllegalStateException("TUN establishment refused");
            setUnderlyingNetworks(new Network[] { physicalNetwork });
            for (int sample = 0; running && sample < 12; sample++) {
                JSONObject result = new JSONObject();
                try (DatagramSocket socket = new DatagramSocket()) {
                    physicalNetwork.bindSocket(socket);
                    boolean protectedSocket = protect(socket);
                    if (!protectedSocket) throw new IllegalStateException("Socket protection refused");
                    socket.setSoTimeout(2000);
                    byte[] payload = ("avian-probe-" + sample).getBytes(StandardCharsets.UTF_8);
                    socket.send(new DatagramPacket(payload, payload.length, InetAddress.getByName("10.0.2.2"), port));
                    byte[] received = new byte[128];
                    DatagramPacket response = new DatagramPacket(received, received.length);
                    socket.receive(response);
                    boolean echoed = new String(received, 0, response.getLength(), StandardCharsets.UTF_8)
                        .equals(new String(payload, StandardCharsets.UTF_8));
                    result.put("sample", sample).put("tun_established", true).put("protected_socket", protectedSocket)
                        .put("network_bound", true).put("echo_received", echoed).put("transport", transport)
                        .put("sdk", android.os.Build.VERSION.SDK_INT);
                    Log.i("AVIAN_PLATFORM", result.toString());
                }
                Thread.sleep(1000);
            }
        } catch (Exception error) {
            Log.e("AVIAN_PLATFORM", "PROBE_FAILED " + error.getClass().getSimpleName() + ": " + error.getMessage());
        }
    }

    @Override public void onRevoke() { stopSelf(); }
    @Override public void onDestroy() {
        running = false;
        if (worker != null) worker.interrupt();
        try { if (tunnel != null) tunnel.close(); } catch (Exception ignored) { }
        super.onDestroy();
    }
}
