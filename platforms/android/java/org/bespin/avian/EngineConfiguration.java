package org.bespin.avian;

import android.content.Context;
import android.net.InetAddresses;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileInputStream;
import java.net.Inet4Address;
import java.net.InetAddress;
import java.nio.charset.StandardCharsets;
import org.json.JSONObject;

final class EngineConfiguration {
  final JSONObject document;
  final InetAddress overlayAddress;
  final InetAddress overlayRoute;
  final int overlayPrefixLength;
  final File credentialFile;

  private EngineConfiguration(Context context, JSONObject document) throws Exception {
    this.document = document;
    overlayAddress = InetAddresses.parseNumericAddress(document.getString("overlay_address"));
    String[] route = document.getString("overlay_network").split("/", -1);
    if (route.length != 2) throw new IllegalArgumentException("Invalid overlay route");
    overlayRoute = InetAddresses.parseNumericAddress(route[0]);
    overlayPrefixLength = Integer.parseInt(route[1]);
    if (!(overlayAddress instanceof Inet4Address)
        || !(overlayRoute instanceof Inet4Address)
        || !overlayAddress.isSiteLocalAddress()
        || !overlayRoute.isSiteLocalAddress()
        || overlayPrefixLength < 8
        || overlayPrefixLength > 32
        || !routeContainsAddress()) {
      throw new IllegalArgumentException("AVIAN requires an assigned private IPv4 overlay route");
    }
    File root = AvianRuntime.storageDirectory(context).getCanonicalFile();
    for (String field :
        new String[] {
          "trust_root_file", "membership_file", "working_directory", "credential_file"
        }) {
      File path = new File(document.getString(field)).getCanonicalFile();
      if (!path.getPath().startsWith(root.getPath() + File.separator)) {
        throw new IllegalArgumentException(
            "AVIAN paths must stay in its private storage directory");
      }
    }
    credentialFile = new File(document.getString("credential_file"));
  }

  private boolean routeContainsAddress() {
    byte[] route = overlayRoute.getAddress();
    byte[] address = overlayAddress.getAddress();
    for (int index = 0; index < 4; index++) {
      int prefixBits = Math.max(0, Math.min(8, overlayPrefixLength - index * 8));
      int mask = (0xFF << (8 - prefixBits)) & 0xFF;
      if ((route[index] & mask) != (address[index] & mask)) return false;
      if ((route[index] & ~mask & 0xFF) != 0) return false;
    }
    return true;
  }

  static EngineConfiguration read(Context context) throws Exception {
    File file = new File(AvianRuntime.storageDirectory(context), "runtime.json");
    try (FileInputStream stream = new FileInputStream(file);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream()) {
      byte[] buffer = new byte[4096];
      int count;
      while ((count = stream.read(buffer)) >= 0) {
        if (bytes.size() + count > 65536)
          throw new java.io.IOException("AVIAN configuration exceeds budget");
        bytes.write(buffer, 0, count);
      }
      return new EngineConfiguration(
          context, new JSONObject(new String(bytes.toByteArray(), StandardCharsets.UTF_8)));
    }
  }
}
