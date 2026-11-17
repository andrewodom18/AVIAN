package org.bespin.avian;

import android.os.Process;
import android.system.Os;
import android.system.OsConstants;
import android.system.StructStat;
import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.FileDescriptor;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Uses the same envelope marker as the shared protected-storage backend. */
public final class ProtectedSecretStore {
  private static final byte[] ENVELOPE =
      "AVIAN-PROTECTED-KEY-v1\n".getBytes(StandardCharsets.US_ASCII);
  private final SecretProtector protector;

  public ProtectedSecretStore(SecretProtector protector) {
    this.protector = protector;
  }

  private static void validate(File file, boolean directory) throws Exception {
    validateMetadata(Os.lstat(file.getAbsolutePath()), directory);
  }

  private static void validateMetadata(StructStat metadata, boolean directory) throws Exception {
    boolean correctType =
        directory ? OsConstants.S_ISDIR(metadata.st_mode) : OsConstants.S_ISREG(metadata.st_mode);
    if (!correctType
        || metadata.st_uid != Process.myUid()
        || (metadata.st_mode & 0077) != 0
        || (!directory && metadata.st_nlink != 1)) {
      throw new java.io.IOException("AVIAN storage requires owner-only regular files");
    }
  }

  public static void validateDirectory(File directory) throws Exception {
    validate(directory, true);
  }

  public byte[] read(File file, int maximumBytes) throws Exception {
    validate(file.getParentFile(), true);
    if (maximumBytes < 0 || maximumBytes > 65536)
      throw new IllegalArgumentException("Invalid secret size budget");
    FileDescriptor descriptor =
        Os.open(file.getAbsolutePath(), OsConstants.O_RDONLY | OsConstants.O_NOFOLLOW, 0);
    byte[] document;
    try (FileInputStream stream = new FileInputStream(descriptor);
        ByteArrayOutputStream bytes = new ByteArrayOutputStream()) {
      validateMetadata(Os.fstat(descriptor), false);
      byte[] buffer = new byte[1024];
      int count;
      while ((count = stream.read(buffer)) >= 0) {
        if (bytes.size() + count > maximumBytes + 65536)
          throw new java.io.IOException("Wrapped secret exceeds budget");
        bytes.write(buffer, 0, count);
      }
      document = bytes.toByteArray();
    }
    if (document.length < ENVELOPE.length
        || !Arrays.equals(ENVELOPE, Arrays.copyOf(document, ENVELOPE.length))) {
      throw new java.io.IOException("A protected key envelope is required");
    }
    byte[] plaintext =
        protector.unprotect(Arrays.copyOfRange(document, ENVELOPE.length, document.length));
    if (plaintext.length > maximumBytes) {
      Arrays.fill(plaintext, (byte) 0);
      throw new java.io.IOException("Secret exceeds budget");
    }
    return plaintext;
  }

  public void create(File file, byte[] plaintext) throws Exception {
    validate(file.getParentFile(), true);
    if (plaintext.length > 65536) throw new IllegalArgumentException("Secret exceeds budget");
    byte[] wrapped = protector.protect(plaintext);
    if (wrapped.length == 0 || Arrays.equals(wrapped, plaintext))
      throw new java.io.IOException("Secret wrapping failed");
    File temporary =
        new File(file.getParentFile(), ".avian-" + java.util.UUID.randomUUID() + ".tmp");
    FileDescriptor descriptor =
        Os.open(
            temporary.getAbsolutePath(),
            OsConstants.O_WRONLY
                | OsConstants.O_CREAT
                | OsConstants.O_EXCL
                | OsConstants.O_NOFOLLOW,
            0600);
    try {
      try (FileOutputStream stream = new FileOutputStream(descriptor)) {
        stream.write(ENVELOPE);
        stream.write(wrapped);
        stream.getFD().sync();
      }
      int failure =
          NativeStorage.commit(
              file.getParentFile().getAbsolutePath(), temporary.getName(), file.getName());
      if (failure != 0) throw new android.system.ErrnoException("renameat2", failure);
    } finally {
      if (temporary.exists()) Os.remove(temporary.getAbsolutePath());
    }
  }
}
