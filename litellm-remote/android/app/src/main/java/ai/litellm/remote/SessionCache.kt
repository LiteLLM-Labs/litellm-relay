package ai.litellm.remote

import android.content.Context
import java.security.MessageDigest
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

@Serializable
internal data class OfflineSessions(
    val devices: List<Device>,
    val selectedDevice: String?,
    val sessions: Map<String, List<CodingSession>>,
)

internal class SessionCache(context: Context) {
    private val preferences = context.getSharedPreferences("remote.sessionLists", Context.MODE_PRIVATE)
    private val json = Json { ignoreUnknownKeys = true }

    private fun key(endpoint: String, gateway: String, subject: String) =
        MessageDigest.getInstance("SHA-256").digest("$endpoint\n$gateway\n$subject".toByteArray())
            .joinToString("") { "%02x".format(it) }

    fun load(endpoint: String, gateway: String, subject: String): OfflineSessions? = runCatching {
        preferences.getString(key(endpoint, gateway, subject), null)?.let { json.decodeFromString<OfflineSessions>(it) }
    }.getOrNull()

    fun save(endpoint: String, gateway: String, subject: String, value: OfflineSessions) {
        val key = key(endpoint, gateway, subject)
        val encoded = json.encodeToString(value)
        if (preferences.getString(key, null) != encoded) preferences.edit().putString(key, encoded).apply()
    }

    fun clear(endpoint: String, gateway: String, subject: String) {
        preferences.edit().remove(key(endpoint, gateway, subject)).apply()
    }
}
