package org.bespin.avian;

import android.app.Activity;
import android.content.Context;
import android.os.Bundle;
import android.system.Os;
import android.system.OsConstants;
import android.util.Log;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileOutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;
import org.json.JSONObject;

/** Installed only in a disposable emulator session by the qualification runner. */
public final class QualificationActivity extends Activity {
  private static volatile boolean probing;
  private static volatile boolean publishEnabled = true;
  private static volatile boolean restarting;
  private static volatile int socketGeneration;
  private static long eventCursor;

  private static void waitForTunnel(Context context) throws Exception {
    long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
    android.net.ConnectivityManager connectivity =
        context.getSystemService(android.net.ConnectivityManager.class);
    while (android.os.SystemClock.elapsedRealtime() < deadline) {
      String encoded = NativeEngine.status();
      if (encoded != null) {
        JSONObject engine = new JSONObject(encoded);
        if ("rejected".equals(engine.optString("tun_state"))) {
          snapshot();
          throw new IllegalStateException("Native TUN setup was rejected");
        }
        boolean vpnVisible = false;
        for (android.net.Network network : connectivity.getAllNetworks()) {
          android.net.NetworkCapabilities capabilities =
              connectivity.getNetworkCapabilities(network);
          if (capabilities != null
              && capabilities.hasTransport(android.net.NetworkCapabilities.TRANSPORT_VPN))
            vpnVisible = true;
        }
        if ("ready".equals(engine.optString("tun_state")) && vpnVisible) {
          record(new JSONObject().put("kind", "application_tun_ready"));
          return;
        }
      }
      Thread.sleep(50);
    }
    snapshot();
    throw new IllegalStateException("Native TUN and VPN network did not become ready");
  }

  private DatagramSocket[] applicationSockets() throws Exception {
    DatagramSocket[] connections = {new DatagramSocket(), new DatagramSocket()};
    for (DatagramSocket connection : connections) {
      connection.setTrafficClass(0xA0);
      connection.setSoTimeout(100);
    }
    return connections;
  }

  private static synchronized void snapshot() throws Exception {
    // Exercise the typed event parser as well as recording the wire page.
    EventPage typed = AvianRuntime.events(eventCursor, 100);
    JSONObject events = new JSONObject(NativeEngine.eventPage(eventCursor, 100));
    eventCursor = typed.nextCursor;
    org.json.JSONArray mainFrames = new org.json.JSONArray();
    Thread main = android.os.Looper.getMainLooper().getThread();
    StackTraceElement[] frames = main.getStackTrace();
    for (int index = 0; index < Math.min(frames.length, 32); index++)
      mainFrames.put(frames[index].toString());
    record(
        new JSONObject()
            .put("kind", "main_thread_snapshot")
            .put("state", main.getState().toString())
            .put("frames", mainFrames));
    record(
        new JSONObject()
            .put("kind", "snapshot")
            .put("elapsed", android.os.SystemClock.elapsedRealtimeNanos() / 1e9)
            .put("services", AvianRuntime.services(null, 100))
            .put("events", events)
            .put("engine", new JSONObject(NativeEngine.status())));
  }

  private static final java.util.concurrent.atomic.AtomicLong sampleIdentifier =
      new java.util.concurrent.atomic.AtomicLong();

  private static void record(JSONObject result) {
    String document = result.toString();
    if (document.getBytes(StandardCharsets.UTF_8).length <= 2800) {
      Log.i("AVIAN_NATIVE_TEST", document);
      return;
    }
    String encoded =
        android.util.Base64.encodeToString(
            document.getBytes(StandardCharsets.UTF_8), android.util.Base64.NO_WRAP);
    long identifier = sampleIdentifier.incrementAndGet();
    int count = (encoded.length() + 1999) / 2000;
    for (int index = 0; index < count; index++) {
      try {
        Log.i(
            "AVIAN_NATIVE_TEST",
            new JSONObject()
                .put("kind", "sample_chunk")
                .put("id", identifier)
                .put("index", index)
                .put("count", count)
                .put(
                    "payload",
                    encoded.substring(index * 2000, Math.min(encoded.length(), (index + 1) * 2000)))
                .toString());
      } catch (org.json.JSONException impossibleForPrimitiveFields) {
        throw new IllegalStateException(impossibleForPrimitiveFields);
      }
    }
  }

  private byte[] asset(String name) throws Exception {
    try (java.io.InputStream stream = getAssets().open(name);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream()) {
      byte[] buffer = new byte[4096];
      int count;
      while ((count = stream.read(buffer)) >= 0) bytes.write(buffer, 0, count);
      return bytes.toByteArray();
    }
  }

  private void writePublic(File file, byte[] bytes) throws Exception {
    java.io.FileDescriptor descriptor =
        Os.open(
            file.getAbsolutePath(),
            OsConstants.O_WRONLY
                | OsConstants.O_CREAT
                | OsConstants.O_EXCL
                | OsConstants.O_NOFOLLOW,
            0600);
    try (FileOutputStream output = new FileOutputStream(descriptor)) {
      output.write(bytes);
      output.getFD().sync();
    }
  }

  @Override
  public void onCreate(Bundle state) {
    super.onCreate(state);
    handleIntent();
  }

  @Override
  protected void onNewIntent(android.content.Intent intent) {
    super.onNewIntent(intent);
    setIntent(intent);
    handleIntent();
  }

  private static final java.util.concurrent.ExecutorService controlWorker =
      java.util.concurrent.Executors.newSingleThreadExecutor();

  /**
   * Only the disposable fixture receiver calls this controller; no production component exports it.
   */
  static void dispatchControl(
      final Context context, final String action, final Runnable completion) {
    controlWorker.execute(
        new Runnable() {
          @Override
          public void run() {
            try {
              record(new JSONObject().put("kind", "action_received").put("action", action));
              if ("pause_publication".equals(action) || "resume_publication".equals(action)) {
                publishEnabled = "resume_publication".equals(action);
                record(
                    new JSONObject()
                        .put("kind", "publication_state")
                        .put("enabled", publishEnabled));
              } else if ("snapshot".equals(action)) {
                snapshot();
              } else if ("restart_runtime".equals(action)) {
                restarting = true;
                AvianRuntime.stop(context);
                long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
                while (AvianVpnService.activeTunnels.get() != 0
                    && android.os.SystemClock.elapsedRealtime() < deadline) Thread.sleep(50);
                if (AvianVpnService.activeTunnels.get() != 0)
                  throw new IllegalStateException("TUN did not close for restart");
                AvianRuntime.start(context);
                waitForTunnel(context);
                socketGeneration++;
                restarting = false;
                record(new JSONObject().put("kind", "runtime_restarted"));
              } else if ("stop".equals(action)) {
                probing = false;
                AvianRuntime.stop(context);
                long deadline = android.os.SystemClock.elapsedRealtime() + 10000;
                while (AvianVpnService.activeTunnels.get() != 0
                    && android.os.SystemClock.elapsedRealtime() < deadline) Thread.sleep(50);
                record(
                    new JSONObject()
                        .put("kind", "cleanup")
                        .put("tun_closed", AvianVpnService.activeTunnels.get() == 0));
              } else throw new IllegalArgumentException("Unknown fixture action");
            } catch (Exception | LinkageError error) {
              try {
                record(
                    new JSONObject()
                        .put("kind", "failure")
                        .put("stage", "control")
                        .put("cleanup_phase", AvianVpnService.cleanupProgress())
                        .put("action", action)
                        .put("error_type", error.getClass().getSimpleName()));
              } catch (Exception ignored) {
              }
            } finally {
              completion.run();
            }
          }
        });
  }

  private void handleIntent() {
    final String action = getIntent().getStringExtra("action");
    new Thread(
            new Runnable() {
              @Override
              public void run() {
                String stage = "initialization";
                try {
                  if (!"start".equals(action) && !"keystore".equals(action)) {
                    dispatchControl(
                        getApplicationContext(),
                        action,
                        new Runnable() {
                          @Override
                          public void run() {
                            finish();
                          }
                        });
                    return;
                  }
                  record(new JSONObject().put("kind", "action_received").put("action", action));
                  File root = AvianRuntime.storageDirectory(QualificationActivity.this);
                  byte[] secret = new byte[32];
                  new java.security.SecureRandom().nextBytes(secret);
                  ProtectedSecretStore store =
                      new ProtectedSecretStore(new KeystoreSecretProtector());
                  File protectedFile = new File(root, "probe-secret");
                  stage = "probe_create";
                  store.create(protectedFile, secret);
                  stage = "probe_read";
                  byte[] restored = store.read(protectedFile, 32);
                  boolean roundTrip = java.security.MessageDigest.isEqual(secret, restored);
                  Arrays.fill(restored, (byte) 0);
                  KeystoreSecretProtector protector = new KeystoreSecretProtector();
                  byte[] first = protector.protect(secret);
                  byte[] second = protector.protect(secret);
                  boolean randomized = !Arrays.equals(first, second);
                  first[first.length - 1] ^= 1;
                  boolean tamperRejected = false;
                  try {
                    protector.unprotect(first);
                  } catch (java.security.GeneralSecurityException expected) {
                    tamperRejected = true;
                  }
                  boolean overwriteRejected = false;
                  try {
                    stage = "probe_create";
                    store.create(protectedFile, secret);
                  } catch (Exception expected) {
                    overwriteRejected = true;
                  }
                  boolean plaintextRejected = false;
                  File plain = new File(root, "probe-plain");
                  stage = "probe_plaintext_write";
                  writePublic(plain, secret);
                  try {
                    store.read(plain, 32);
                  } catch (Exception expected) {
                    plaintextRejected = true;
                  }
                  Arrays.fill(secret, (byte) 0);
                  record(
                      new JSONObject()
                          .put("kind", "keystore")
                          .put("round_trip", roundTrip)
                          .put("randomized_ciphertext", randomized)
                          .put("tamper_rejected", tamperRejected)
                          .put("overwrite_rejected", overwriteRejected)
                          .put("plaintext_rejected", plaintextRejected));
                  if ("keystore".equals(action)) {
                    finish();
                    return;
                  }
                  File runtime = new File(root, "runtime");
                  Os.mkdir(runtime.getAbsolutePath(), 0700);
                  writePublic(new File(root, "trust-root.json"), asset("trust-root.json"));
                  writePublic(new File(root, "membership.json"), asset("membership.json"));
                  byte[] credential = asset("transport.key");
                  store.create(new File(root, "transport.key"), credential);
                  Arrays.fill(credential, (byte) 0);
                  byte[] identity = asset("identity.key");
                  store.create(new File(root, "identity.key"), identity);
                  Arrays.fill(identity, (byte) 0);
                  JSONObject configuration =
                      new JSONObject(new String(asset("runtime.json"), StandardCharsets.UTF_8));
                  configuration.put(
                      "trust_root_file", new File(root, "trust-root.json").getAbsolutePath());
                  configuration.put(
                      "membership_file", new File(root, "membership.json").getAbsolutePath());
                  configuration.put(
                      "credential_file", new File(root, "transport.key").getAbsolutePath());
                  configuration.put("working_directory", runtime.getAbsolutePath());
                  writePublic(
                      new File(root, "runtime.json"),
                      configuration.toString().getBytes(StandardCharsets.UTF_8));
                  java.net.URI underlay =
                      java.net.URI.create(configuration.getJSONArray("peers").getString(0));
                  boolean underlayReachable = false;
                  try (java.net.Socket probe = new java.net.Socket()) {
                    probe.connect(
                        new java.net.InetSocketAddress(underlay.getHost(), underlay.getPort()),
                        1000);
                    underlayReachable = true;
                  } catch (java.io.IOException expected) {
                  }
                  record(
                      new JSONObject()
                          .put("kind", "underlay_tcp_probe")
                          .put("connected", underlayReachable));
                  stage = "vpn_start";
                  AvianRuntime.start(QualificationActivity.this);
                  waitForTunnel(QualificationActivity.this);
                  probing = true;
                  long lastPublication = 0;
                  long lastSnapshot = 0;
                  DatagramSocket[] connections = applicationSockets();
                  int observedGeneration = socketGeneration;
                  try {
                    for (int sequence = 0; probing && sequence < 3000; sequence++) {
                      if (restarting) {
                        Thread.sleep(100);
                        continue;
                      }
                      if (observedGeneration != socketGeneration) {
                        for (DatagramSocket connection : connections) connection.close();
                        connections = applicationSockets();
                        observedGeneration = socketGeneration;
                      }
                      long elapsed = android.os.SystemClock.elapsedRealtime();
                      if (!restarting && publishEnabled && elapsed - lastPublication >= 2000) {
                        try {
                          PublishedServices publication =
                              AvianRuntime.registerServices(
                                  QualificationActivity.this,
                                  java.util.Collections.singletonList(
                                      new ServiceRegistration(
                                          ServiceRegistration.Kind.ARC_TELEMETRY,
                                          ServiceRegistration.Protocol.UDP,
                                          19000)),
                                  5);
                          record(
                              new JSONObject()
                                  .put("kind", "local_publication")
                                  .put("revision", publication.revision)
                                  .put("owner_node_id", publication.ownerNodeId)
                                  .put("expires_at_unix", publication.expiresAtUnix));
                          lastPublication = elapsed;
                        } catch (Exception unavailableDuringStartup) {
                        }
                      }
                      for (int index = 0; index < connections.length; index++) {
                        DatagramSocket connection = connections[index];
                        InetAddress peer = InetAddress.getByName("10.201.0." + (index + 2));
                        byte[] payload = ("native-" + sequence).getBytes(StandardCharsets.US_ASCII);
                        boolean received = false;
                        double attempted = android.os.SystemClock.elapsedRealtimeNanos() / 1e9;
                        Double receivedAt = null;
                        int receivedSequence = -1;
                        try {
                          connection.send(new DatagramPacket(payload, payload.length, peer, 19000));
                          DatagramPacket response = new DatagramPacket(new byte[128], 128);
                          connection.receive(response);
                          double actualReceipt =
                              android.os.SystemClock.elapsedRealtimeNanos() / 1e9;
                          String echoed =
                              new String(
                                  response.getData(),
                                  0,
                                  response.getLength(),
                                  StandardCharsets.US_ASCII);
                          if (response.getAddress().equals(peer)
                              && response.getPort() == 19000
                              && echoed.startsWith("native-")) {
                            receivedSequence = Integer.parseInt(echoed.substring(7));
                            received = receivedSequence >= 0 && receivedSequence <= sequence;
                            if (received) receivedAt = actualReceipt;
                          }
                        } catch (java.io.IOException | NumberFormatException expected) {
                        }
                        record(
                            new JSONObject()
                                .put("kind", "overlay_echo")
                                .put("sequence", sequence)
                                .put("peer", peer.getHostAddress())
                                .put("received", received)
                                .put("received_sequence", receivedSequence)
                                .put("attempt_elapsed", attempted)
                                .put(
                                    "receipt_elapsed",
                                    receivedAt == null ? JSONObject.NULL : receivedAt)
                                .put(
                                    "completed_elapsed",
                                    android.os.SystemClock.elapsedRealtimeNanos() / 1e9)
                                .put("prepared_sockets", AvianVpnService.preparedSockets.get())
                                .put("active_tuns", AvianVpnService.activeTunnels.get()));
                      }
                      if (!restarting && elapsed - lastSnapshot >= 1000) {
                        try {
                          snapshot();
                          lastSnapshot = elapsed;
                        } catch (Exception unavailableDuringStartup) {
                        }
                      }
                      Thread.sleep(100);
                    }
                  } finally {
                    for (DatagramSocket connection : connections) connection.close();
                  }
                  finish();
                } catch (Exception | LinkageError error) {
                  try {
                    JSONObject failure =
                        new JSONObject()
                            .put("kind", "failure")
                            .put("error_type", error.getClass().getSimpleName())
                            .put("stage", stage);
                    if (error instanceof android.system.ErrnoException) {
                      android.system.ErrnoException system = (android.system.ErrnoException) error;
                      failure.put("errno", system.errno);
                      String operation = system.getMessage().split(" failed:", 2)[0];
                      if (operation.matches("[a-z_0-9]{1,32}")) failure.put("function", operation);
                    }
                    record(failure);
                  } catch (Exception ignored) {
                  }
                  finish();
                }
              }
            },
            "avian-native-qualification")
        .start();
  }
}
