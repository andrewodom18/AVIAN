package org.bespin.avian;

import org.json.JSONException;
import org.json.JSONObject;

/** A declared local endpoint. Its class hint never changes packet classification or policy. */
public final class ServiceRegistration {
  public enum Kind {
    ARC_CONTROL("arc_control"),
    ARC_TELEMETRY("arc_telemetry"),
    ARC_VIDEO("arc_video"),
    ARC_TRANSFER("arc_transfer");
    final String identifier;

    Kind(String identifier) {
      this.identifier = identifier;
    }
  }

  public enum Protocol {
    TCP("tcp"),
    UDP("udp");
    final String identifier;

    Protocol(String identifier) {
      this.identifier = identifier;
    }
  }

  public final Kind kind;
  public final Protocol protocol;
  public final int port;

  public ServiceRegistration(Kind kind, Protocol protocol, int port) {
    if (kind == null || protocol == null || port < 1 || port > 65535) {
      throw new IllegalArgumentException("Invalid local service endpoint");
    }
    this.kind = kind;
    this.protocol = protocol;
    this.port = port;
  }

  JSONObject document() throws JSONException {
    return new JSONObject()
        .put("kind", kind.identifier)
        .put("protocol", protocol.identifier)
        .put("port", port);
  }
}
