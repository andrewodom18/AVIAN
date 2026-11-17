package org.bespin.avian.probe;

import android.app.Activity;
import android.content.Intent;
import android.net.VpnService;
import android.os.Bundle;

public final class ProbeActivity extends Activity {
    @Override public void onCreate(Bundle saved) {
        super.onCreate(saved);
        Intent permission = VpnService.prepare(this);
        if (permission != null) {
            android.util.Log.e("AVIAN_PLATFORM", "VPN_PERMISSION_REQUIRED");
            finish();
            return;
        }
        Intent service = new Intent(this, ProbeService.class);
        service.putExtra("port", getIntent().getIntExtra("port", 0));
        startForegroundService(service);
        finish();
    }
}
