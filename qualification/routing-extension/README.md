# Routing metadata integrity gate

This test-only patch exercises the pinned engine's real Secure Mode handshake,
encryption filter, tunnel, and receiving connection. It first delivers an
untouched baseline, then changes the routing hint **after encryption**. The
receiver must reject the altered packet. No packet-forwarding changes are made.

Build the image with `qualification/Dockerfile.build`, then run:

```sh
python3 qualification/routing-extension/check_integrity.py \
  --source /path/to/easytier-v2.6.4 \
  --output /tmp/avian-routing-integrity.json
```

The runner verifies the pinned revision and applies the patch to a disposable
clone. It returns 0 for a passed gate, 2 for the reproduced integrity failure,
and 1 for a build or execution error. Output includes the test log.

The pinned engine accepts the changed hint while decrypting the original
payload. This does **not** demonstrate altered application plaintext; it shows
that relay-visible routing metadata cannot be trusted merely because the
payload is encrypted.

