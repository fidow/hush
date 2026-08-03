package com.fidow.hush

import android.os.Bundle
import android.view.View
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

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
}
