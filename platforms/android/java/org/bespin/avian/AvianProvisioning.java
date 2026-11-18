package org.bespin.avian;

import android.content.Context;
import android.util.Base64;
import java.io.File;
import java.nio.charset.StandardCharsets;
import java.security.SecureRandom;
import java.util.Arrays;

/** Offline device setup. Public request/bundle files contain no private credentials. */
public final class AvianProvisioning {
  static { System.loadLibrary("avian_android"); }
  private AvianProvisioning() {}

  private static native String provision(
      int operation, String directory, String document, String fingerprint,
      byte[] identity, byte[] transport);

  /** Creates device-owned credentials once and returns a signed public enrollment request. */
  public static synchronized String createRequest(Context context) throws Exception {
    File directory = AvianRuntime.storageDirectory(context);
    File identityFile = new File(directory, "identity.key");
    File transportFile = new File(directory, "transport.key");
    if (identityFile.exists() != transportFile.exists())
      throw new IllegalStateException("Incomplete device credentials; restore complete device state");
    ProtectedSecretStore store = new ProtectedSecretStore(new KeystoreSecretProtector());
    if (!identityFile.exists()) {
      byte[] identity = new byte[32];
      byte[] transport = new byte[32];
      byte[] encodedIdentity = null;
      byte[] encodedTransport = null;
      try {
        SecureRandom random = new SecureRandom();
        random.nextBytes(identity);
        random.nextBytes(transport);
        encodedIdentity = hexadecimal(identity);
        encodedTransport = Base64.encode(transport, Base64.NO_WRAP);
        store.create(identityFile, encodedIdentity);
        store.create(transportFile, encodedTransport);
      } finally {
        Arrays.fill(identity, (byte) 0);
        Arrays.fill(transport, (byte) 0);
        if (encodedIdentity != null) Arrays.fill(encodedIdentity, (byte) 0);
        if (encodedTransport != null) Arrays.fill(encodedTransport, (byte) 0);
      }
    }
    return invoke(context, 0, "", "");
  }

  /** Requires an issuer fingerprint verified independently of the imported bundle. */
  public static synchronized String importBundle(
      Context context, String bundle, String administratorFingerprint) throws Exception {
    if (administratorFingerprint == null || administratorFingerprint.isEmpty())
      throw new IllegalArgumentException("An independently verified administrator fingerprint is required");
    return invoke(context, 1, bundle, administratorFingerprint);
  }

  /** Installs explicit overlay/peer settings without requesting consent or starting the VPN. */
  public static synchronized String configureRuntime(Context context, String profile) throws Exception {
    return invoke(context, 2, profile, "");
  }

  private static String invoke(Context context, int operation, String document, String fingerprint)
      throws Exception {
    if (document == null || document.length() > 16 * 1024 * 1024)
      throw new IllegalArgumentException("Provisioning document exceeds resource budget");
    File directory = AvianRuntime.storageDirectory(context);
    ProtectedSecretStore store = new ProtectedSecretStore(new KeystoreSecretProtector());
    byte[] identity = store.read(new File(directory, "identity.key"), 65);
    byte[] transport = null;
    try {
      transport = store.read(new File(directory, "transport.key"), 65);
      String result = provision(operation, directory.getCanonicalPath(), document, fingerprint, identity, transport);
      if (result == null)
        throw new IllegalStateException("Provisioning rejected; stop AVIAN and verify enrollment, fingerprint, and profile");
      return result;
    } finally {
      Arrays.fill(identity, (byte) 0);
      if (transport != null) Arrays.fill(transport, (byte) 0);
    }
  }

  private static byte[] hexadecimal(byte[] bytes) {
    byte[] digits = "0123456789abcdef".getBytes(StandardCharsets.US_ASCII);
    byte[] encoded = new byte[bytes.length * 2];
    for (int index = 0; index < bytes.length; index++) {
      encoded[index * 2] = digits[(bytes[index] & 0xFF) >>> 4];
      encoded[index * 2 + 1] = digits[bytes[index] & 0x0F];
    }
    return encoded;
  }
}
