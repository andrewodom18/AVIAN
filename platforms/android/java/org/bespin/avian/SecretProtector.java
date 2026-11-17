package org.bespin.avian;

import java.security.GeneralSecurityException;

/** Host implementation of the private-storage secret wrapping contract. */
public interface SecretProtector {
  byte[] protect(byte[] plaintext) throws GeneralSecurityException;

  byte[] unprotect(byte[] protectedBytes) throws GeneralSecurityException;
}
