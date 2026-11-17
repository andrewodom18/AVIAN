package org.bespin.avian;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;
import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

/** Compare epoch on reconnect; gap requires a fresh authoritative service snapshot. */
public final class EventPage {
  public final String epoch;
  public final boolean gap;
  public final long nextCursor;
  public final long latestSequence;
  public final List<RuntimeEvent> items;

  EventPage(String encoded) throws JSONException {
    JSONObject document = new JSONObject(encoded);
    epoch = document.getString("epoch");
    gap = document.getBoolean("gap");
    nextCursor = document.getLong("next_cursor");
    latestSequence = document.getLong("latest_sequence");
    JSONArray entries = document.getJSONArray("items");
    List<RuntimeEvent> parsed = new ArrayList<>();
    for (int index = 0; index < entries.length(); index++)
      parsed.add(new RuntimeEvent(entries.getJSONObject(index)));
    items = Collections.unmodifiableList(parsed);
  }
}
