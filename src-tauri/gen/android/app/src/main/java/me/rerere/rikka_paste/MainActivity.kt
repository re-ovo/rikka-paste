package me.rerere.rikka_paste

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Bundle
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  /** Android 默认丢弃发给本机的组播包，mDNS 发现设备需要持有这把锁 */
  private var multicastLock: WifiManager.MulticastLock? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    // 在 Rust 端启动 mDNS 之前拿到，避免错过最初几次查询的响应
    val wifi = applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager
    multicastLock = wifi.createMulticastLock("rikka-paste").apply {
      setReferenceCounted(false)
      acquire()
    }
    super.onCreate(savedInstanceState)
  }

  override fun onDestroy() {
    multicastLock?.release()
    super.onDestroy()
  }
}
