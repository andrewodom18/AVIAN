package org.bespin.avian;

import java.util.Locale;
import org.json.JSONException;
import org.json.JSONObject;

public final class RuntimeEvent {
  public enum Kind {
    RUNTIME_STARTED,
    TUN_READY,
    TUN_REJECTED,
    RUNTIME_STOPPED,
    UNDERLAYS_RECONCILED,
    UNDERLAY_UPDATE_REJECTED,
    EXCHANGE_LISTENING,
    MEMBERSHIP_UPDATED,
    LOCAL_SERVICES_PUBLISHED,
    SERVICE_RECORDS_RECEIVED,
    REVOCATIONS_APPLIED,
    SERVICE_UPDATE_REJECTED,
    REVOCATION_UPDATE_REJECTED,
    EXCHANGE_UNAVAILABLE,
    CAPACITY_PROBE_COMPLETED,
    CAPACITY_PROBE_REJECTED,
    EXCHANGE_WORKER_FAILED
  }

  public final JSONObject detail;
  public final long sequence;
  public final Kind kind;
  public final long atUnix;
  public final long revocationRevision;

  RuntimeEvent(JSONObject document) throws JSONException {
    detail = document.optJSONObject("detail");
    sequence = document.getLong("sequence");
    kind = Kind.valueOf(document.getString("kind").toUpperCase(Locale.ROOT));
    atUnix = document.getLong("at_unix");
    revocationRevision = document.getLong("revocation_revision");
  }
}
