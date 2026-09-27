package me.rerere.rikka_paste

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.os.PersistableBundle
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

/** ClipDescription.EXTRA_IS_SENSITIVE，常量从 API 33 起才有，低版本上带着也无害 */
private const val EXTRA_IS_SENSITIVE = "android.content.extra.IS_SENSITIVE"
private const val LABEL = "Rikka Paste"

@InvokeArg
class ReadArgs {
  lateinit var imagePath: String
}

@InvokeArg
class WriteArgs {
  var text: String? = null
  var imagePath: String? = null
  var sensitive: Boolean = false
}

/** 系统剪贴板读写，对应 Rust 侧的 src/clipboard/android.rs */
@TauriPlugin
class ClipboardPlugin(private val activity: Activity) : Plugin(activity) {
  private val clipboard = activity.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager

  /** 同时有文本和图片时只取文本；Android 10 起只有应用在前台且有焦点时才读得到 */
  @Command
  fun read(invoke: Invoke) {
    val args = invoke.parseArgs(ReadArgs::class.java)
    val clip = clipboard.primaryClip
    if (clip == null || clip.itemCount == 0) {
      invoke.resolve(result("empty"))
      return
    }
    if (clip.description.extras?.getBoolean(EXTRA_IS_SENSITIVE) == true) {
      invoke.resolve(result("sensitive"))
      return
    }
    val item = clip.getItemAt(0)
    val text = item.text?.toString()
    if (!text.isNullOrEmpty()) {
      invoke.resolve(result("text").put("text", text))
      return
    }
    val uri = item.uri
    val type = uri?.let { activity.contentResolver.getType(it) }
    if (uri == null || type?.startsWith("image/") != true) {
      invoke.resolve(result("empty"))
      return
    }
    // 图片可能有几十 MB，读取和转码不占用主线程
    Thread {
      try {
        val input = activity.contentResolver.openInputStream(uri) ?: error("无法打开 $uri")
        input.use {
          File(args.imagePath).outputStream().use { output ->
            if (type == "image/png") {
              // PNG 原样复制，与其他平台一样保持字节不变
              input.copyTo(output)
            } else {
              val bitmap = BitmapFactory.decodeStream(input) ?: error("无法解码 $type 图片")
              bitmap.compress(Bitmap.CompressFormat.PNG, 100, output)
            }
          }
        }
        invoke.resolve(result("image"))
      } catch (e: Exception) {
        invoke.reject("读取剪贴板图片失败: ${e.message}")
      }
    }.start()
  }

  /** 图片通过 FileProvider 的 content:// URI 写入，粘贴时系统会给目标应用临时授予读取权限 */
  @Command
  fun write(invoke: Invoke) {
    val args = invoke.parseArgs(WriteArgs::class.java)
    val text = args.text
    val imagePath = args.imagePath
    val clip = when {
      text != null -> ClipData.newPlainText(LABEL, text)
      imagePath != null -> {
        val uri = FileProvider.getUriForFile(activity, "${activity.packageName}.fileprovider", File(imagePath))
        ClipData.newUri(activity.contentResolver, LABEL, uri)
      }
      else -> {
        invoke.reject("没有要写入的内容")
        return
      }
    }
    if (args.sensitive) {
      clip.description.extras = PersistableBundle().apply { putBoolean(EXTRA_IS_SENSITIVE, true) }
    }
    clipboard.setPrimaryClip(clip)
    invoke.resolve()
  }

  private fun result(kind: String) = JSObject().put("kind", kind)
}
