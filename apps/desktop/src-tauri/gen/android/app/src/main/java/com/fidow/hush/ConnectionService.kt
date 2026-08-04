package com.fidow.hush

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder

/// Keeps Hush running while the app is in the background.
///
/// Messages arrive over a connection held open by the Rust engine, which lives
/// in this process. Android stops processes it considers idle, and the
/// connection dies with them, so without a foreground service a message sent
/// while the app is not on screen would only turn up the next time it is
/// opened. A foreground service is the way to say "this process is doing
/// something for the user", and the price Android charges for it is a
/// notification the user can see.
class ConnectionService : Service() {
  override fun onBind(intent: Intent?): IBinder? = null

  override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
    startForeground(NOTIFICATION_ID, buildNotification())
    // Restarted if the system kills it under memory pressure.
    return START_STICKY
  }

  /// Swiping the app away is the user saying they are done: the connection
  /// goes with it rather than lingering as a notification they did not ask
  /// for.
  override fun onTaskRemoved(rootIntent: Intent?) {
    stopSelf()
    super.onTaskRemoved(rootIntent)
  }

  private fun buildNotification(): Notification {
    val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      val channel = NotificationChannel(
        CHANNEL_ID,
        getString(R.string.connection_channel_name),
        // Low: it belongs in the shade, not in the user's face.
        NotificationManager.IMPORTANCE_LOW,
      )
      channel.setShowBadge(false)
      manager.createNotificationChannel(channel)
    }

    val open = PendingIntent.getActivity(
      this,
      0,
      Intent(this, MainActivity::class.java),
      PendingIntent.FLAG_IMMUTABLE,
    )

    return Notification.Builder(this, CHANNEL_ID)
      .setContentTitle(getString(R.string.connection_title))
      .setContentText(getString(R.string.connection_text))
      .setSmallIcon(R.drawable.ic_notification)
      .setContentIntent(open)
      .setOngoing(true)
      .build()
  }

  companion object {
    private const val CHANNEL_ID = "hush-connection"
    private const val NOTIFICATION_ID = 1

    fun start(context: Context) {
      val intent = Intent(context, ConnectionService::class.java)
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        context.startForegroundService(intent)
      } else {
        context.startService(intent)
      }
    }
  }
}
