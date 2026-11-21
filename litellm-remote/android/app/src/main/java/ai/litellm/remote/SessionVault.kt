package ai.litellm.remote

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

interface SessionStorage {
    fun save(workspace: String, token: String): Boolean
    fun load(workspace: String): String?
    fun remove(workspace: String): Boolean
}

class SessionVault(context: Context) : SessionStorage {
    private val preferences = context.getSharedPreferences("remote.sessions", Context.MODE_PRIVATE)
    private val alias = "ai.litellm.remote.session"
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }

    private fun id(workspace: String) = MessageDigest.getInstance("SHA-256").digest(workspace.toByteArray()).joinToString("") { "%02x".format(it) }

    override fun save(workspace: String, token: String): Boolean = runCatching {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.ENCRYPT_MODE, key()) }
        val encrypted = cipher.iv + cipher.doFinal(token.toByteArray(Charsets.UTF_8))
        preferences.edit().putString(id(workspace), Base64.encodeToString(encrypted, Base64.NO_WRAP)).commit()
    }.getOrDefault(false)

    override fun load(workspace: String): String? = runCatching {
        val value = preferences.getString(id(workspace), null) ?: return null
        val bytes = Base64.decode(value, Base64.NO_WRAP)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply { init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0, 12))) }
        String(cipher.doFinal(bytes.copyOfRange(12, bytes.size)), Charsets.UTF_8)
    }.getOrNull()

    override fun remove(workspace: String): Boolean = preferences.edit().remove(id(workspace)).commit()
}
