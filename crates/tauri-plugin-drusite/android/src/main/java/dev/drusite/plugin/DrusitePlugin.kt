// SPDX-License-Identifier: AGPL-3.0-or-later

package dev.drusite.plugin

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.provider.Settings
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.security.keystore.StrongBoxUnavailableException
import android.util.Base64
import androidx.appcompat.app.AppCompatActivity
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@InvokeArg
class AliasArgs {
  lateinit var alias: String
}

@InvokeArg
class SignArgs {
  lateinit var alias: String
  lateinit var message: String
}

@InvokeArg
class AuthenticateArgs {
  lateinit var url: String
  lateinit var scheme: String
}

/// The Android half of the Drusite plugin: the device key and the store key in the Android
/// Keystore, and signing in through the browser.
@TauriPlugin
class DrusitePlugin(private val activity: Activity) : Plugin(activity) {
  private val keyStore: KeyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
  private val main = Handler(Looper.getMainLooper())
  private var pending: Invoke? = null
  private var scheme: String? = null

  private fun b64(bytes: ByteArray) = Base64.encodeToString(bytes, Base64.NO_WRAP)

  // ---- Device key ----

  private fun keyName(alias: String) = "dev.drusite.dpop.$alias"

  private fun generate(alias: String, strongBox: Boolean) {
    val spec = KeyGenParameterSpec.Builder(keyName(alias), KeyProperties.PURPOSE_SIGN)
      .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
      .setDigests(KeyProperties.DIGEST_SHA256)
    if (strongBox && Build.VERSION.SDK_INT >= 28) spec.setIsStrongBoxBacked(true)
    KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore")
      .apply { initialize(spec.build()) }
      .generateKeyPair()
  }

  /// The P-256 key for `alias`, made on first use: in StrongBox (a separate secure chip) where
  /// the phone has one, else the TEE. Its private half can't be read out.
  @Command
  fun deviceKey(invoke: Invoke) {
    val args = invoke.parseArgs(AliasArgs::class.java)
    try {
      if (!keyStore.containsAlias(keyName(args.alias))) {
        try {
          generate(args.alias, true)
        } catch (e: StrongBoxUnavailableException) {
          generate(args.alias, false)
        }
      }
      val pub = keyStore.getCertificate(keyName(args.alias)).publicKey as ECPublicKey
      val ret = JSObject()
      // BigInteger bytes: may carry a sign byte or drop leading zeros; Rust evens them out.
      ret.put("x", b64(pub.w.affineX.toByteArray()))
      ret.put("y", b64(pub.w.affineY.toByteArray()))
      invoke.resolve(ret)
    } catch (e: Exception) {
      invoke.reject("can't make the device key: ${e.javaClass.simpleName}: ${e.message}")
    }
  }

  /// An ECDSA P-256 signature (DER) over the base64 `message`.
  @Command
  fun sign(invoke: Invoke) {
    val args = invoke.parseArgs(SignArgs::class.java)
    try {
      val key = keyStore.getKey(keyName(args.alias), null) as? PrivateKey
      if (key == null) {
        invoke.reject("no device key")
        return
      }
      val signature = Signature.getInstance("SHA256withECDSA").run {
        initSign(key)
        update(Base64.decode(args.message, Base64.DEFAULT))
        sign()
      }
      val ret = JSObject()
      ret.put("signature", b64(signature))
      invoke.resolve(ret)
    } catch (e: Exception) {
      invoke.reject("can't sign: ${e.javaClass.simpleName}: ${e.message}")
    }
  }

  // ---- Store key ----

  private val wrapName = "dev.drusite.store.wrap"

  private fun wrappingKey(): SecretKey {
    (keyStore.getKey(wrapName, null) as? SecretKey)?.let { return it }
    val spec = KeyGenParameterSpec.Builder(
      wrapName, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
      .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
      .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
      .setKeySize(256)
      .build()
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
      .apply { init(spec) }
      .generateKey()
  }

  /// The local store's key: 32 random bytes, kept on disk only wrapped (AES-GCM) by a Keystore
  /// key, in the app's no-backup folder. Made on first use.
  @Command
  fun storeKey(invoke: Invoke) {
    try {
      val file = File(activity.noBackupFilesDir, "store-key")
      val key: ByteArray
      if (file.exists()) {
        val bytes = file.readBytes()
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, wrappingKey(), GCMParameterSpec(128, bytes, 0, 12))
        key = cipher.doFinal(bytes, 12, bytes.size - 12)
      } else {
        key = ByteArray(32).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, wrappingKey())
        val sealed = cipher.iv + cipher.doFinal(key)
        val temp = File(activity.noBackupFilesDir, "store-key.new")
        temp.writeBytes(sealed)
        if (!temp.renameTo(file)) throw IllegalStateException("can't save the store key")
      }
      val ret = JSObject()
      ret.put("key", key.joinToString("") { "%02x".format(it) })
      key.fill(0)
      invoke.resolve(ret)
    } catch (e: Exception) {
      invoke.reject("can't open the store key: ${e.javaClass.simpleName}: ${e.message}")
    }
  }

  @Command
  fun deviceName(invoke: Invoke) {
    val named = Settings.Global.getString(activity.contentResolver, Settings.Global.DEVICE_NAME)
    val ret = JSObject()
    ret.put("name", named?.takeIf { it.isNotBlank() } ?: "${Build.MANUFACTURER} ${Build.MODEL}")
    invoke.resolve(ret)
  }

  // ---- Signing in ----

  /// Opens `url` in the browser; resolves when the browser sends the app back on `scheme`
  /// (through the intent filter in the app's manifest), or with `cancelled` when the person
  /// comes back to the app without finishing.
  @Command
  fun authenticate(invoke: Invoke) {
    val args = invoke.parseArgs(AuthenticateArgs::class.java)
    main.post {
      pending?.resolve(JSObject().put("cancelled", true))
      pending = invoke
      scheme = args.scheme
      try {
        activity.startActivity(
          Intent(Intent.ACTION_VIEW, Uri.parse(args.url)).addCategory(Intent.CATEGORY_BROWSABLE))
      } catch (e: Exception) {
        pending = null
        invoke.reject("can't open the browser: ${e.message}")
      }
    }
  }

  @Command
  fun cancelAuthentication(invoke: Invoke) {
    main.post {
      pending?.resolve(JSObject().put("cancelled", true))
      pending = null
      invoke.resolve()
    }
  }

  override fun onNewIntent(intent: Intent) {
    val data = intent.data ?: return
    if (data.scheme != scheme) return
    main.post {
      pending?.resolve(JSObject().put("url", data.toString()))
      pending = null
    }
  }

  override fun onResume(activity: AppCompatActivity) {
    // A redirect arrives (onNewIntent) before the app resumes. Still waiting a moment after
    // resuming means the person came back by hand.
    main.postDelayed({
      pending?.resolve(JSObject().put("cancelled", true))
      pending = null
    }, 1500)
  }
}
