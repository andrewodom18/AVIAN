package org.bespin.avian;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Explicit shell-only control for the disposable test APK, absent from the production library. */
public final class QualificationReceiver extends BroadcastReceiver {
  @Override
  public void onReceive(Context context, Intent intent) {
    final PendingResult pending = goAsync();
    QualificationActivity.dispatchControl(
        context.getApplicationContext(),
        intent.getStringExtra("action"),
        new Runnable() {
          @Override
          public void run() {
            pending.finish();
          }
        });
  }
}
