package __PACKAGE__

import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.webkit.WebView
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat

// MixDeck's Android screen. Differences from the default Tauri activity:
//  * keeps the web view (and the music playing in it) running when the app leaves the
//    screen, with a foreground service so Android doesn't stop the app;
//  * keeps the interface clear of the status and navigation bars.
class MainActivity : TauriActivity() {
  private var web: WebView? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    super.onCreate(savedInstanceState)
    window.decorView.setBackgroundColor(0xFF0F0F14.toInt())
    WindowCompat.getInsetsController(window, window.decorView).apply {
      isAppearanceLightStatusBars = false
      isAppearanceLightNavigationBars = false
    }
    ViewCompat.setOnApplyWindowInsetsListener(findViewById(android.R.id.content)) { v, insets ->
      val b = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      v.setPadding(b.left, b.top, b.right, b.bottom)
      WindowInsetsCompat.CONSUMED
    }
    if (Build.VERSION.SDK_INT >= 33) {
      requestPermissions(arrayOf(android.Manifest.permission.POST_NOTIFICATIONS), 1)
    }
    startPlaybackService()
  }

  private fun startPlaybackService() {
    val i = Intent(this, PlaybackService::class.java)
    try {
      if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
    } catch (e: Exception) {
      android.util.Log.w("MixDeck", "couldn't start the playback service", e)
    }
  }

  override fun onWebViewCreate(webView: WebView) {
    web = webView
    webView.settings.mediaPlaybackRequiresUserGesture = false
    if (Build.VERSION.SDK_INT >= 26) {
      webView.setRendererPriorityPolicy(WebView.RENDERER_PRIORITY_IMPORTANT, false)
    }
  }

  // The base activity pauses the web view here, which would stop the music.
  override fun onPause() {
    super.onPause()
    web?.onResume()
    web?.resumeTimers()
  }

  override fun onStop() {
    super.onStop()
    web?.onResume()
    web?.resumeTimers()
  }

  override fun onDestroy() {
    if (isFinishing) stopService(Intent(this, PlaybackService::class.java))
    super.onDestroy()
  }
}
