package com.fidow.hush

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.os.Build
import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    createMessageChannels()

    // Edge to edge means the window covers the whole screen, status bar and
    // navigation bar included, so without this the conversation header sits
    // under the clock and the composer under the navigation buttons. Padding
    // the content by the system insets keeps the interface inside the usable
    // area, and since the keyboard is one of those insets, the composer rises
    // with it instead of hiding behind it.
    // Without this the process is stopped soon after the app leaves the
    // screen, and with it the connection that messages arrive on.
    ConnectionService.start(this)

    val content = findViewById<View>(android.R.id.content)
    ViewCompat.setOnApplyWindowInsetsListener(content) { view, windowInsets ->
      val insets = windowInsets.getInsets(
        WindowInsetsCompat.Type.systemBars()
          or WindowInsetsCompat.Type.displayCutout()
          or WindowInsetsCompat.Type.ime()
      )
      view.setPadding(insets.left, insets.top, insets.right, insets.bottom)
      WindowInsetsCompat.CONSUMED
    }
  }

  /// One channel per way of being alerted, because a channel's sound and
  /// vibration cannot be changed once Android has seen it. Which one a message
  /// uses is the user's choice in settings.
  ///
  /// Sound and vibration are left to Android, which is what makes a phone on
  /// silent stay silent — a tone played by the app itself would ignore that.
  private fun createMessageChannels() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
    val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager

    val sound = NotificationChannel(
      CHANNEL_SOUND,
      getString(R.string.channel_messages_sound),
      NotificationManager.IMPORTANCE_HIGH,
    )
    sound.enableVibration(true)

    val vibrate = NotificationChannel(
      CHANNEL_VIBRATE,
      getString(R.string.channel_messages_vibrate),
      NotificationManager.IMPORTANCE_HIGH,
    )
    vibrate.setSound(null, null)
    vibrate.enableVibration(true)

    val silent = NotificationChannel(
      CHANNEL_SILENT,
      getString(R.string.channel_messages_silent),
      NotificationManager.IMPORTANCE_LOW,
    )
    silent.setSound(null, null)
    silent.enableVibration(false)

    manager.createNotificationChannels(listOf(sound, vibrate, silent))
  }

  companion object {
    const val CHANNEL_SOUND = "hush-messages-sound"
    const val CHANNEL_VIBRATE = "hush-messages-vibrate"
    const val CHANNEL_SILENT = "hush-messages-silent"
  }
}
