package __PACKAGE__

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.wifi.WifiManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager

// Keeps MixDeck running (and playing) while it's in the background or the screen is off.
// Android requires a visible notification for this. Swiping MixDeck away from recent apps
// stops it.
class PlaybackService : Service() {
  private var wake: PowerManager.WakeLock? = null
  private var wifi: WifiManager.WifiLock? = null

  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    val nm = getSystemService(NotificationManager::class.java)
    if (Build.VERSION.SDK_INT >= 26) {
      val ch = NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW)
      ch.description = "Keeps MixDeck playing in the background"
      ch.setShowBadge(false)
      nm.createNotificationChannel(ch)
    }
    val open = PendingIntent.getActivity(
      this, 0,
      Intent(this, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
      PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT
    )
    @Suppress("DEPRECATION")
    val builder = if (Build.VERSION.SDK_INT >= 26) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
    val n = builder
      .setContentTitle("MixDeck")
      .setContentText("Running. Tap to open, or swipe MixDeck away from recent apps to stop.")
      .setSmallIcon(android.R.drawable.ic_media_play)
      .setContentIntent(open)
      .setOngoing(true)
      .build()
    if (Build.VERSION.SDK_INT >= 29) {
      startForeground(1, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)
    } else {
      startForeground(1, n)
    }
    if (wake == null) {
      wake = (getSystemService(Context.POWER_SERVICE) as PowerManager)
        .newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "MixDeck::playback")
        .apply { setReferenceCounted(false); acquire() }
    }
    if (wifi == null) {
      @Suppress("DEPRECATION")
      val mode = if (Build.VERSION.SDK_INT >= 29) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else WifiManager.WIFI_MODE_FULL_HIGH_PERF
      wifi = (applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager)
        .createWifiLock(mode, "MixDeck::stream")
        .apply { setReferenceCounted(false); acquire() }
    }
    return START_NOT_STICKY
  }

  override fun onTaskRemoved(rootIntent: Intent?) {
    stopSelf()
    super.onTaskRemoved(rootIntent)
  }

  override fun onDestroy() {
    try { wake?.release() } catch (_: Exception) {}
    try { wifi?.release() } catch (_: Exception) {}
    super.onDestroy()
  }

  companion object {
    const val CHANNEL = "mixdeck_playback"
  }
}
