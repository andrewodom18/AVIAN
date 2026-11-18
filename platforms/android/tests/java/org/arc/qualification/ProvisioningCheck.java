package org.arc.qualification;

import android.app.Instrumentation;
import android.content.Context;
import android.content.Intent;
import android.os.Bundle;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;

public final class ProvisioningCheck extends Instrumentation {
  private Bundle arguments;

  @Override
  public void onCreate(Bundle arguments) {
    this.arguments = arguments;
    start();
  }

  @Override
  public void onStart() {
    Bundle result = new Bundle();
    try {
      Context context = getTargetContext();
      String action = arguments.getString("action", "request");
      Class<?> provisioning =
          context.getClassLoader().loadClass("org.bespin.avian.AvianProvisioning");
      if (action.equals("request")) {
        String request =
            (String) provisioning.getMethod("createRequest", Context.class).invoke(null, context);
        try (FileOutputStream stream =
            new FileOutputStream(new File(context.getFilesDir(), "enrollment-request.json"))) {
          stream.write(request.getBytes(StandardCharsets.UTF_8));
        }
        String repeated =
            (String) provisioning.getMethod("createRequest", Context.class).invoke(null, context);
        if (!request.equals(repeated))
          throw new AssertionError("Device request changed across repeated setup");
        result.putString("request_stable", "passed");
      } else if (action.equals("import")) {
        String bundle =
            new String(
                Files.readAllBytes(
                    new File(context.getFilesDir(), "enrollment-bundle.json").toPath()),
                StandardCharsets.UTF_8);
        String fingerprint = arguments.getString("fingerprint");
        boolean rejected = false;
        try {
          provisioning
              .getMethod("importBundle", Context.class, String.class, String.class)
              .invoke(
                  null,
                  context,
                  bundle,
                  (fingerprint.charAt(0) == '0' ? "1" : "0") + fingerprint.substring(1));
        } catch (java.lang.reflect.InvocationTargetException error) {
          rejected = true;
        }
        if (!rejected) throw new AssertionError("Wrong administrator fingerprint accepted");
        if (new File(context.getFilesDir(), "avian/membership.json").exists())
          throw new AssertionError("Rejected bundle changed device membership");
        String imported =
            (String)
                provisioning
                    .getMethod("importBundle", Context.class, String.class, String.class)
                    .invoke(null, context, bundle, fingerprint);
        provisioning
            .getMethod("importBundle", Context.class, String.class, String.class)
            .invoke(null, context, bundle, fingerprint);
        String profile =
            "{\"overlay_address\":\"10.88.0.2\",\"overlay_network\":\"10.88.0.0/24\",\"peers\":[],\"control_tcp_ports\":[7447]}";
        provisioning
            .getMethod("configureRuntime", Context.class, String.class)
            .invoke(null, context, profile);
        File storage = new File(context.getFilesDir(), "avian");
        if (new File(storage, "administrator.key").exists())
          throw new AssertionError("Administrator key installed on device");
        for (String name : new String[] {"identity.key", "transport.key"}) {
          byte[] protectedBytes = Files.readAllBytes(new File(storage, name).toPath());
          if (!new String(protectedBytes, 0, 23, StandardCharsets.US_ASCII)
              .startsWith("AVIAN-PROTECTED-KEY-v1"))
            throw new AssertionError("Plaintext credential persisted");
        }
        String runtime =
            new String(
                Files.readAllBytes(new File(storage, "runtime.json").toPath()),
                StandardCharsets.UTF_8);
        org.json.JSONObject runtimeConfiguration = new org.json.JSONObject(runtime);
        if (runtimeConfiguration.getJSONArray("control_tcp_ports").getInt(0) != 7447)
          throw new AssertionError("Explicit control port was not retained");
        result.putString("explicit_control_port", "passed");
        result.putString("wrong_fingerprint_rejected", "passed");
        result.putString("bundle_repeat_import", "passed");
        result.putString("protected_credentials", "passed");
        result.putString("runtime_profile", "passed");
        result.putString("enrollment", imported);
      } else if (action.equals("runtime")) {
        Intent intent = new Intent();
        intent.setClassName(context.getPackageName(), "ai.rownd.arc.SettingsActivity");
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        startActivitySync(intent);
        ClassLoader loader = context.getClassLoader();
        Class<?> mesh = loader.loadClass("ai.rownd.arc.AvianMesh");
        Object controller = mesh.getField("INSTANCE").get(null);
        Class<?> runtime = loader.loadClass("org.bespin.avian.AvianRuntime");
        context
            .getSharedPreferences("arc_wrapper_prefs", Context.MODE_PRIVATE)
            .edit()
            .putBoolean("avianSelected", true)
            .putBoolean("avianEnabled", true)
            .commit();
        try {
          mesh.getMethod("start", Context.class).invoke(controller, context);
          waitForRecords(0);
          Class<?> kind = loader.loadClass("org.bespin.avian.ServiceRegistration$Kind");
          Class<?> protocol = loader.loadClass("org.bespin.avian.ServiceRegistration$Protocol");
          Object endpoint =
              loader
                  .loadClass("org.bespin.avian.ServiceRegistration")
                  .getConstructor(kind, protocol, int.class)
                  .newInstance(
                      kind.getField("ARC_CONTROL").get(null),
                      protocol.getField("TCP").get(null),
                      7447);
          runtime
              .getMethod("registerServices", Context.class, java.util.List.class, int.class)
              .invoke(null, context, java.util.Collections.singletonList(endpoint), 300);
          org.json.JSONArray records = waitForRecords(1).getJSONArray("records");
          org.json.JSONObject candidate = records.getJSONObject(0);
          if (!candidate.getString("authorization_status").equals("unauthorized")
              || !candidate.isNull("drone_id"))
            throw new AssertionError("Network membership authorized an aircraft identity");
          if (!candidate
              .getJSONArray("endpoints")
              .getJSONObject(0)
              .getString("source")
              .equals("avian"))
            throw new AssertionError("AVIAN service did not reach native fleet discovery");
          String peerBefore = readyPeerId(runtime);
          Intent mainIntent = new Intent();
          mainIntent.setClassName(context.getPackageName(), "ai.rownd.arc.MainActivity");
          mainIntent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
          android.app.Activity activity = startActivitySync(mainIntent);
          try (java.net.Socket connection = new java.net.Socket("127.0.0.1", 9101)) {
            connection.setSoTimeout(3000);
            requireBridgeResponse(connection);
            ActivityMonitor monitor = addMonitor("ai.rownd.arc.MainActivity", null, false);
            runOnMainSync(
                new Runnable() {
                  @Override
                  public void run() {
                    activity.recreate();
                  }
                });
            android.app.Activity replacement = waitForMonitorWithTimeout(monitor, 15000);
            if (replacement == null || replacement == activity)
              throw new AssertionError("Main activity did not actually recreate");
            requireBridgeResponse(connection);
          }
          String peerAfter = readyPeerId(runtime);
          if (!peerBefore.equals(peerAfter))
            throw new AssertionError("Activity recreation restarted the mesh engine");
          waitForRecords(1);
          result.putString("activity_recreation_preserves_bridge_and_mesh", "passed");
          runtime
              .getMethod("registerServices", Context.class, java.util.List.class, int.class)
              .invoke(null, context, java.util.Collections.emptyList(), 300);
          waitForRecords(0);
          result.putString("native_fleet_ingress", "passed");
          result.putString("aircraft_authorization_preserved", "passed");
          result.putString("service_withdrawal", "passed");
          mesh.getMethod("stopInteractively", Context.class).invoke(controller, context);
          context
              .getSharedPreferences("arc_wrapper_prefs", Context.MODE_PRIVATE)
              .edit()
              .putBoolean("avianEnabled", false)
              .commit();
          result.putString("interactive_stop_without_aircraft", "passed");
        } catch (Throwable failure) {
          result.putString(
              "mesh_status", String.valueOf(mesh.getMethod("getStatusText").invoke(controller)));
          try {
            result.putString(
                "native_status", String.valueOf(runtime.getMethod("status").invoke(null)));
          } catch (Throwable unavailable) {
            result.putString("native_status", "unavailable: " + unavailable);
          }
          throw failure;
        } finally {
          context
              .getSharedPreferences("arc_wrapper_prefs", Context.MODE_PRIVATE)
              .edit()
              .putBoolean("avianEnabled", false)
              .commit();
          mesh.getMethod("stop", Context.class).invoke(controller, context);
        }
        boolean stopped = false;
        for (int attempt = 0; attempt < 50; attempt++) {
          try {
            runtime.getMethod("status").invoke(null);
          } catch (java.lang.reflect.InvocationTargetException expected) {
            stopped = true;
            break;
          }
          Thread.sleep(100);
        }
        if (!stopped) throw new AssertionError("Native mesh runtime remained active after stop");
        result.putString("native_stop", "passed");
      } else if (action.equals("show")) {
        Intent intent = new Intent();
        intent.setClassName(context.getPackageName(), "ai.rownd.arc.SettingsActivity");
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        startActivitySync(intent);
        Bundle visible = new Bundle();
        visible.putString("settings_resumed", "true");
        sendStatus(1, visible);
        File finished = new File(context.getFilesDir(), "finish-ui-check");
        long deadline = android.os.SystemClock.elapsedRealtime() + 120000;
        while (!finished.exists() && android.os.SystemClock.elapsedRealtime() < deadline)
          Thread.sleep(200);
        finished.delete();
        result.putString("settings", "shown");
      } else {
        throw new IllegalArgumentException("Unknown qualification action");
      }
      finish(-1, result);
    } catch (Throwable error) {
      result.putString("failure", error.toString());
      if (error.getCause() != null) result.putString("cause", error.getCause().toString());
      finish(0, result);
    }
  }

  private String readyPeerId(Class<?> runtime) throws Exception {
    org.json.JSONObject status = (org.json.JSONObject) runtime.getMethod("status").invoke(null);
    if (!status.getString("tun_state").equals("ready") || status.isNull("peer_id"))
      throw new AssertionError("Native mesh instance is not ready");
    return status.get("peer_id").toString();
  }

  private void requireBridgeResponse(java.net.Socket connection) throws Exception {
    connection
        .getOutputStream()
        .write(
            ("HEAD /api/fleet HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n")
                .getBytes(StandardCharsets.US_ASCII));
    connection.getOutputStream().flush();
    StringBuilder headers = new StringBuilder();
    while (headers.length() < 16384 && !headers.toString().endsWith("\r\n\r\n")) {
      int value = connection.getInputStream().read();
      if (value < 0)
        throw new AssertionError("Activity recreation closed the existing bridge connection");
      headers.append((char) value);
    }
    if (!headers.toString().startsWith("HTTP/1.1 200 "))
      throw new AssertionError("Native bridge did not answer on the original connection");
  }

  private org.json.JSONObject waitForRecords(int expected) throws Exception {
    Throwable last = null;
    for (int attempt = 0; attempt < 100; attempt++) {
      java.net.HttpURLConnection connection = null;
      try {
        connection =
            (java.net.HttpURLConnection)
                new java.net.URL("http://127.0.0.1:9101/api/fleet").openConnection();
        connection.setConnectTimeout(200);
        connection.setReadTimeout(500);
        try (java.io.InputStream stream = connection.getInputStream();
            java.io.ByteArrayOutputStream bytes = new java.io.ByteArrayOutputStream()) {
          byte[] buffer = new byte[4096];
          int count;
          while ((count = stream.read(buffer)) >= 0) bytes.write(buffer, 0, count);
          org.json.JSONObject snapshot =
              new org.json.JSONObject(new String(bytes.toByteArray(), StandardCharsets.UTF_8));
          if (snapshot.getJSONArray("records").length() == expected) return snapshot;
        }
      } catch (Exception error) {
        last = error;
      } finally {
        if (connection != null) connection.disconnect();
      }
      Thread.sleep(100);
    }
    throw new AssertionError("Native fleet directory did not reach " + expected + " records", last);
  }
}
