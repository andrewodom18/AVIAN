package org.bespin.avian;

import android.content.Context;
import android.content.Intent;
import android.net.VpnService;
import android.system.Os;
import java.io.File;
import java.util.Arrays;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONObject;

/** Native component for a host application's existing VPN-consent flow. */
public final class AvianRuntime {
  private AvianRuntime() {}

  public static Intent consentIntent(Context context) {
    return VpnService.prepare(context);
  }

  public static File storageDirectory(Context context) throws Exception {
    File directory = new File(context.getFilesDir(), "avian");
    if (!directory.exists()) {
      if (!directory.mkdir())
        throw new java.io.IOException("Cannot create AVIAN private directory");
      Os.chmod(directory.getAbsolutePath(), 0700);
    }
    ProtectedSecretStore.validateDirectory(directory);
    return directory;
  }

  /** Invoke only after the host has received VPN consent from the user. */
  public static void start(Context context) {
    if (VpnService.prepare(context) != null)
      throw new IllegalStateException("VPN consent is required");
    AvianVpnService.requestStart(context);
  }

  public static void stop(Context context) {
    AvianVpnService.requestStop(context);
  }

  /**
   * Refresh while the owner application wants these endpoints advertised; an empty list withdraws
   * them.
   */
  public static PublishedServices registerServices(
      Context context, List<ServiceRegistration> endpoints, int ttlSeconds) throws Exception {
    if (ttlSeconds < 1 || ttlSeconds > 300)
      throw new IllegalArgumentException("Service TTL must be within 1..300 seconds");
    JSONArray records = new JSONArray();
    for (ServiceRegistration endpoint : endpoints) records.put(endpoint.document());
    String document =
        new JSONObject().put("ttl_seconds", ttlSeconds).put("endpoints", records).toString();
    byte[] identity =
        new ProtectedSecretStore(new KeystoreSecretProtector())
            .read(new File(storageDirectory(context), "identity.key"), 65);
    try {
      String signed = NativeEngine.publishServices(document, identity);
      if (signed == null) throw new IllegalStateException("Local service publication was rejected");
      return new PublishedServices(signed);
    } finally {
      Arrays.fill(identity, (byte) 0);
    }
  }

  public static JSONObject services(String after, int limit) throws Exception {
    String page = NativeEngine.servicePage(after, limit);
    if (page == null) throw new IllegalStateException("Service snapshot is unavailable");
    return new JSONObject(page);
  }

  public static EventPage events(long after, int limit) throws Exception {
    String page = NativeEngine.eventPage(after, limit);
    if (page == null) throw new IllegalStateException("Runtime events are unavailable");
    return new EventPage(page);
  }
}
