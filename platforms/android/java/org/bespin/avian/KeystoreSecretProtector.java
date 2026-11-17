package org.bespin.avian;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import java.security.GeneralSecurityException;
import java.security.KeyStore;
import java.util.Arrays;
import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** The wrapping key stays in Android Keystore; wrapped data never falls back to plaintext. */
public final class KeystoreSecretProtector implements SecretProtector {
  private static final String ALIAS = "avian.owner-secrets.v1";
  private static final byte[] CONTEXT =
      "AVIAN-SECRET-1".getBytes(java.nio.charset.StandardCharsets.US_ASCII);

  private static synchronized SecretKey key(boolean create) throws GeneralSecurityException {
    try {
      KeyStore store = KeyStore.getInstance("AndroidKeyStore");
      store.load(null);
      if (store.containsAlias(ALIAS)) {
        return (SecretKey) store.getKey(ALIAS, null);
      }
      if (!create) throw new GeneralSecurityException("AVIAN wrapping key is unavailable");
      KeyGenerator generator =
          KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
      generator.init(
          new KeyGenParameterSpec.Builder(
                  ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
              .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
              .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
              .setKeySize(256)
              .setRandomizedEncryptionRequired(true)
              .build());
      return generator.generateKey();
    } catch (java.io.IOException error) {
      throw new GeneralSecurityException("Android Keystore is unavailable", error);
    }
  }

  @Override
  public synchronized byte[] protect(byte[] plaintext) throws GeneralSecurityException {
    Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
    cipher.init(Cipher.ENCRYPT_MODE, key(true));
    cipher.updateAAD(CONTEXT);
    byte[] encrypted = cipher.doFinal(plaintext);
    byte[] nonce = cipher.getIV();
    if (nonce.length != 12) throw new GeneralSecurityException("Unexpected GCM nonce size");
    byte[] document = Arrays.copyOf(nonce, nonce.length + encrypted.length);
    System.arraycopy(encrypted, 0, document, nonce.length, encrypted.length);
    return document;
  }

  @Override
  public synchronized byte[] unprotect(byte[] protectedBytes) throws GeneralSecurityException {
    if (protectedBytes.length < 28) throw new GeneralSecurityException("Invalid wrapped secret");
    Cipher cipher = Cipher.getInstance("AES/GCM/NoPadding");
    cipher.init(Cipher.DECRYPT_MODE, key(false), new GCMParameterSpec(128, protectedBytes, 0, 12));
    cipher.updateAAD(CONTEXT);
    return cipher.doFinal(protectedBytes, 12, protectedBytes.length - 12);
  }
}
