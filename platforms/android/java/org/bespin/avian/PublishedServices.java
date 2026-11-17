package org.bespin.avian;

import org.json.JSONException;
import org.json.JSONObject;

/** A persisted signed advertisement; this does not assert endpoint reachability. */
public final class PublishedServices {
  public final String ownerNodeId;
  public final long revision;
  public final long expiresAtUnix;
  private final String signedRecord;

  PublishedServices(String signedRecord) throws JSONException {
    JSONObject claims = new JSONObject(signedRecord).getJSONObject("claims");
    ownerNodeId = claims.getString("owner_node_id");
    revision = claims.getLong("revision");
    expiresAtUnix = claims.getLong("expires_at_unix");
    this.signedRecord = signedRecord;
  }

  public JSONObject signedRecord() throws JSONException {
    return new JSONObject(signedRecord);
  }
}
