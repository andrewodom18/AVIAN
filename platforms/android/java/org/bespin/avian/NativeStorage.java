package org.bespin.avian;

/** Atomic storage primitives unavailable through the Android Java filesystem API. */
final class NativeStorage {
  static {
    System.loadLibrary("avian_storage_android");
  }

  private NativeStorage() {}

  static native int commit(String directory, String temporaryName, String destinationName);
}
