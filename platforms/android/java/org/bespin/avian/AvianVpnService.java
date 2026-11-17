package org.bespin.avian;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.VpnService;
import android.os.Build;
import android.os.ParcelFileDescriptor;
import android.system.OsConstants;
import java.net.InetAddress;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.List;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.TimeUnit;
import org.json.JSONArray;
import org.json.JSONObject;

/** Owns the TUN and engine lifecycle. It contains no fleet or vehicle procedures. */
public final class AvianVpnService extends VpnService {
  static final java.util.concurrent.atomic.AtomicInteger preparedSockets =
      new java.util.concurrent.atomic.AtomicInteger();
  static final java.util.concurrent.atomic.AtomicInteger activeTunnels =
      new java.util.concurrent.atomic.AtomicInteger();
  private static final java.util.concurrent.atomic.AtomicReference<AvianVpnService> currentService =
      new java.util.concurrent.atomic.AtomicReference<>();
  private static final java.util.concurrent.atomic.AtomicLong requestedGeneration =
      new java.util.concurrent.atomic.AtomicLong();
  private static final android.os.Handler mainHandler =
      new android.os.Handler(android.os.Looper.getMainLooper());
  private volatile String cleanupPhase = "not_requested";
  private final ConcurrentHashMap<Network, LinkProperties> networks = new ConcurrentHashMap<>();
  private final ScheduledExecutorService worker = Executors.newSingleThreadScheduledExecutor();
  private ConnectivityManager connectivity;
  private EngineConfiguration configuration;
  private ParcelFileDescriptor tunnel;

  private static final class ApprovedNetworks {
    final long generation;
    final java.util.Map<Network, LinkProperties> properties;
    final java.util.Set<Network> metered;

    ApprovedNetworks(
        long generation,
        java.util.Map<Network, LinkProperties> properties,
        java.util.Set<Network> metered) {
      this.generation = generation;
      this.properties = java.util.Collections.unmodifiableMap(new java.util.HashMap<>(properties));
      this.metered = java.util.Collections.unmodifiableSet(new java.util.HashSet<>(metered));
    }
  }

  private volatile long networkGeneration;
  private volatile ApprovedNetworks approvedNetworks;
  private ApprovedNetworks pendingNetworks;
  private volatile boolean acceptingSockets;
  private volatile boolean stopping;
  private boolean engineRunning;
  private boolean retryUnderlays;
  private long engineSession;
  private static final Object ENGINE_OWNERSHIP = new Object();
  private volatile Network previousDefault;
  private ScheduledFuture<?> pendingRestart;
  private ConnectivityManager.NetworkCallback networkCallback;
  private boolean networkCallbackRegistered;

  @Override
  public void onCreate() {
    super.onCreate();
    currentService.set(this);
  }

  static void requestStart(final android.content.Context context) {
    final long generation = requestedGeneration.incrementAndGet();
    final android.content.Context application = context.getApplicationContext();
    mainHandler.post(
        new Runnable() {
          @Override
          public void run() {
            if (requestedGeneration.get() != generation) return;
            AvianVpnService previous = currentService.get();
            if ((previous != null && previous.stopping)
                || (previous == null && activeTunnels.get() != 0)) {
              // A bound VPN service must finish closing its interface before reuse.
              mainHandler.postDelayed(this, 50);
              return;
            }
            application.startForegroundService(new Intent(application, AvianVpnService.class));
          }
        });
  }

  static void requestStop(android.content.Context context) {
    requestedGeneration.incrementAndGet();
    AvianVpnService active = currentService.get();
    if (active == null) context.stopService(new Intent(context, AvianVpnService.class));
    else active.requestShutdown();
  }

  static String cleanupProgress() {
    AvianVpnService active = currentService.get();
    return active == null ? "absent" : active.cleanupPhase;
  }

  private void requestShutdown() {
    mainHandler.post(
        new Runnable() {
          @Override
          public void run() {
            beginShutdown();
          }
        });
  }

  @Override
  public int onStartCommand(Intent intent, int flags, int identifier) {
    if (stopping) {
      stopSelf();
      return START_NOT_STICKY;
    }
    if (tunnel != null) return START_NOT_STICKY;
    try {
      if (VpnService.prepare(this) != null)
        throw new IllegalStateException("VPN consent was withdrawn");
      configuration = EngineConfiguration.read(this);
      connectivity = getSystemService(ConnectivityManager.class);
      previousDefault = connectivity.getActiveNetwork();
      for (Network network : connectivity.getAllNetworks()) remember(network);
      Network[] underlying = networks.keySet().toArray(new Network[0]);
      tunnel =
          new Builder()
              .setSession("AVIAN")
              .setMtu(1240)
              .addAddress(configuration.overlayAddress, 32)
              .addRoute(configuration.overlayRoute, configuration.overlayPrefixLength)
              .allowFamily(OsConstants.AF_INET6)
              .setUnderlyingNetworks(underlying)
              .setBlocking(false)
              .establish();
      if (tunnel == null) throw new IllegalStateException("VPN establishment was refused");
      activeTunnels.incrementAndGet();
      NotificationManager notifications = getSystemService(NotificationManager.class);
      notifications.createNotificationChannel(
          new NotificationChannel(
              "avian", "AVIAN connectivity", NotificationManager.IMPORTANCE_LOW));
      Notification notification =
          new Notification.Builder(this, "avian")
              .setContentTitle("AVIAN network service")
              .setSmallIcon(android.R.drawable.stat_sys_upload)
              .build();
      if (Build.VERSION.SDK_INT >= 34)
        startForeground(1, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_SYSTEM_EXEMPTED);
      else startForeground(1, notification);
      acceptingSockets = false;
      networkCallback =
          new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
              remember(network);
              changed();
            }

            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties properties) {
              remember(network);
              changed();
            }

            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities capabilities) {
              remember(network);
              changed();
            }

            @Override
            public void onLost(Network network) {
              networks.remove(network);
              changed();
            }
          };
      connectivity.registerNetworkCallback(
          new NetworkRequest.Builder()
              .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
              .build(),
          networkCallback);
      networkCallbackRegistered = true;
      worker.execute(
          new Runnable() {
            @Override
            public void run() {
              reconcileEngine();
            }
          });
      worker.scheduleWithFixedDelay(
          new Runnable() {
            @Override
            public void run() {
              if (!stopping && engineRunning) {
                if (!NativeEngine.refreshTrust(engineSession)) requestShutdown();
                else if (retryUnderlays) reconcileEngine();
              }
            }
          },
          1,
          1,
          TimeUnit.SECONDS);
    } catch (Exception error) {
      requestShutdown();
    }
    return START_NOT_STICKY;
  }

  private void remember(Network network) {
    if (connectivity == null) return;
    NetworkCapabilities capabilities = connectivity.getNetworkCapabilities(network);
    LinkProperties properties = connectivity.getLinkProperties(network);
    if (capabilities != null
        && !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)
        && properties != null) {
      networks.put(network, properties);
    } else networks.remove(network);
  }

  private synchronized void changed() {
    if (stopping) return;
    networkGeneration++;
    acceptingSockets = false;
    Network active = connectivity.getActiveNetwork();
    if (active != null && networks.containsKey(active)) previousDefault = active;
    if (pendingRestart != null) pendingRestart.cancel(false);
    if (!worker.isShutdown())
      pendingRestart =
          worker.schedule(
              new Runnable() {
                @Override
                public void run() {
                  reconcileEngine();
                }
              },
              500,
              TimeUnit.MILLISECONDS);
  }

  private void reconcileEngine() {
    synchronized (ENGINE_OWNERSHIP) {
      reconcileOwnedEngine();
    }
  }

  private void reconcileOwnedEngine() {
    if (stopping) return;
    byte[] credential = null;
    try {
      long generation = networkGeneration;
      java.util.Map<Network, LinkProperties> snapshot = new java.util.HashMap<>(networks);
      java.util.Set<Network> meteredNetworks = new java.util.HashSet<>();
      setUnderlyingNetworks(snapshot.keySet().toArray(new Network[0]));
      if (snapshot.isEmpty() && !engineRunning) return;
      JSONArray metered = new JSONArray();
      JSONArray assignments = new JSONArray();
      for (Network network : snapshot.keySet()) {
        LinkProperties physical = snapshot.get(network);
        if (physical != null && physical.getInterfaceName() != null) {
          for (LinkAddress address : physical.getLinkAddresses()) {
            if (!address.getAddress().isLoopbackAddress()
                && !address.getAddress().isLinkLocalAddress())
              assignments.put(
                  new JSONObject()
                      .put("address", address.getAddress().getHostAddress().split("%", 2)[0])
                      .put("interface", physical.getInterfaceName())
                      .put("network_id", network.getNetworkHandle()));
          }
        }
        NetworkCapabilities capabilities = connectivity.getNetworkCapabilities(network);
        if (capabilities == null
            || !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)) {
          meteredNetworks.add(network);
          LinkProperties properties = snapshot.get(network);
          if (properties != null)
            for (LinkAddress address : properties.getLinkAddresses()) {
              metered.put(address.getAddress().getHostAddress().split("%", 2)[0]);
            }
        }
      }
      pendingNetworks = new ApprovedNetworks(generation, snapshot, meteredNetworks);
      if (!NativeEngine.setUnderlayPolicy(
          new JSONObject().put("metered_addresses", metered).toString())) {
        throw new IllegalStateException("Underlay policy was rejected");
      }
      if (!engineRunning) {
        credential =
            new ProtectedSecretStore(new KeystoreSecretProtector())
                .read(configuration.credentialFile, 65);
        java.io.File policy =
            new java.io.File(
                configuration.document.getString("working_directory"),
                "policy-" + System.nanoTime() + ".json");
        JSONObject startup =
            new JSONObject(configuration.document.toString())
                .put("policy_file", policy.getAbsolutePath());
        if (stopping) return;
        engineSession =
            NativeEngine.start(startup.toString(), credential, this, policy.getAbsolutePath());
        if (engineSession <= 0) throw new IllegalStateException("Engine start rejected");
        int descriptor = ParcelFileDescriptor.dup(tunnel.getFileDescriptor()).detachFd();
        if (!NativeEngine.attachTun(engineSession, descriptor))
          throw new IllegalStateException("Native TUN attachment failed");
        engineRunning = true;
      }
      JSONObject update =
          new JSONObject().put("assignments", assignments).put("metered_addresses", metered);
      int result = NativeEngine.updateUnderlays(engineSession, update.toString());
      if (result < 0) throw new IllegalStateException("Physical interface reconciliation failed");
      retryUnderlays = result > 0;
    } catch (Exception | LinkageError error) {
      try {
        NativeEngine.stop(engineSession);
      } catch (LinkageError ignored) {
      }
      requestShutdown();
    } finally {
      if (credential != null) Arrays.fill(credential, (byte) 0);
    }
  }

  /** Called after the replacement metering policy is durable and active. */
  public synchronized boolean policyActivated() {
    if (stopping || pendingNetworks == null || pendingNetworks.generation != networkGeneration)
      return false;
    approvedNetworks = pendingNetworks;
    acceptingSockets = true;
    return true;
  }

  /** Called synchronously by native socket creation before connect or the first datagram. */
  public boolean prepareSocket(
      int descriptor, String destination, String sourceAddress, String interfaceName) {
    ApprovedNetworks approved = approvedNetworks;
    if (!acceptingSockets
        || stopping
        || approved == null
        || approved.generation != networkGeneration) return false;
    try {
      List<Network> matching = new ArrayList<>();
      for (Network network : approved.properties.keySet()) {
        LinkProperties properties = approved.properties.get(network);
        if (properties == null) continue;
        if (!interfaceName.isEmpty() && !interfaceName.equals(properties.getInterfaceName()))
          continue;
        if (!sourceAddress.isEmpty()) {
          InetAddress expected = android.net.InetAddresses.parseNumericAddress(sourceAddress);
          boolean assigned = false;
          for (LinkAddress address : properties.getLinkAddresses()) {
            if (address.getAddress().equals(expected)) assigned = true;
          }
          if (!assigned) continue;
        }
        matching.add(network);
      }
      Network selected;
      if (matching.contains(previousDefault) && sourceAddress.isEmpty() && interfaceName.isEmpty())
        selected = previousDefault;
      else if (matching.size() == 1) selected = matching.get(0);
      else return false;
      NetworkCapabilities current = connectivity.getNetworkCapabilities(selected);
      if (current == null
          || (!current.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
              && !approved.metered.contains(selected))) return false;
      if (approved.generation != networkGeneration) return false;
      try (ParcelFileDescriptor duplicate = ParcelFileDescriptor.fromFd(descriptor)) {
        selected.bindSocket(duplicate.getFileDescriptor());
        boolean protectedSocket = protect(descriptor);
        if (protectedSocket) preparedSockets.incrementAndGet();
        return protectedSocket;
      }
    } catch (Exception error) {
      return false;
    }
  }

  @Override
  public void onRevoke() {
    requestedGeneration.incrementAndGet();
    requestShutdown();
  }

  private void beginShutdown() {
    if (stopping) return;
    stopping = true;
    acceptingSockets = false;
    cleanupPhase = "queued";
    if (connectivity != null && networkCallbackRegistered) {
      try {
        connectivity.unregisterNetworkCallback(networkCallback);
      } catch (IllegalArgumentException ignored) {
      }
      networkCallbackRegistered = false;
    }
    worker.execute(
        new Runnable() {
          @Override
          public void run() {
            synchronized (ENGINE_OWNERSHIP) {
              try {
                cleanupPhase = "native_stopping";
                NativeEngine.stop(engineSession);
                cleanupPhase = "native_stopped";
              } catch (LinkageError ignored) {
              } finally {
                cleanupPhase = "closing_tun";
                if (tunnel != null) {
                  try {
                    tunnel.close();
                  } catch (java.io.IOException ignored) {
                  }
                  tunnel = null;
                  activeTunnels.decrementAndGet();
                }
                engineRunning = false;
                cleanupPhase = "closed";
                // The system binds the VPN while its interface exists. Closing the
                // descriptor precedes stopSelf, rather than waiting for onDestroy.
                stopForeground(STOP_FOREGROUND_REMOVE);
                stopSelf();
              }
            }
          }
        });
    worker.shutdown();
  }

  @Override
  public void onDestroy() {
    beginShutdown();
    currentService.compareAndSet(this, null);
    super.onDestroy();
  }
}
