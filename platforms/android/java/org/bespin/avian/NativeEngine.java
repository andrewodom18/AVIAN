package org.bespin.avian;

final class NativeEngine {
  static {
    System.loadLibrary("avian_android");
  }

  private NativeEngine() {}

  static native long start(
      String publicConfiguration,
      byte[] transportCredential,
      AvianVpnService callback,
      String policyFile);

  /** Transfers ownership of one duplicated descriptor, including on failure. */
  static native boolean attachTun(long session, int descriptor);

  static native boolean refreshTrust(long session);

  static native void stop(long session);

  static native boolean setUnderlayPolicy(String policy);

  static native int updateUnderlays(long session, String document);

  static native String publishServices(String registration, byte[] ownerIdentity);

  static native String servicePage(String after, int limit);

  static native String eventPage(long after, int limit);

  static native String status();
}
