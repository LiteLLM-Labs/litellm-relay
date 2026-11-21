package ai.litellm.remote

import java.net.HttpURLConnection
import java.net.URI
import java.net.URLEncoder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.withContext
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject

fun workspaceAddress(text: String, gateway: Boolean = false): String? = runCatching {
    val input = text.trim().let { if ("://" in it) it else "https://$it" }
    val uri = URI(input)
    require(uri.port == -1 || uri.port in 1..65535)
    val loopback = uri.host in listOf("localhost", "127.0.0.1", "[::1]")
    require(!uri.host.isNullOrBlank() && uri.rawUserInfo == null && uri.rawQuery == null && uri.rawFragment == null)
    require(uri.scheme == "https" || (!gateway && uri.scheme == "http" && loopback))
    require(gateway || uri.path.isNullOrEmpty() || uri.path == "/")
    val path = uri.path.orEmpty().trimEnd('/').let { if (gateway) it.removeSuffix("/v1") else it }
    URI(uri.scheme, null, uri.host, uri.port, path, null, null).toString()
}.getOrNull()

class ApiClient(val endpoint: String, private val token: () -> String?) {
    val json = Json { ignoreUnknownKeys = true }

    suspend inline fun <reified T> request(
        path: String, method: String = "GET", body: Map<String, String> = emptyMap(),
    ): RemoteResult<T> = when (val result = exchange(path, method, body)) {
        is RemoteResult.Failure -> result
        is RemoteResult.Success -> withContext(Dispatchers.Default) {
            runCatching { RemoteResult.Success(json.decodeFromString<T>(result.value)) }
                .getOrElse { RemoteResult.Failure("Update the app to connect to this workspace.") }
        }
    }

    suspend fun exchange(path: String, method: String, body: Map<String, String>): RemoteResult<String> = withContext(Dispatchers.IO) {
        val base = URI(endpoint)
        val uri = base.resolve(path)
        if (uri.host != base.host || uri.port != base.port || uri.scheme != base.scheme) {
            return@withContext RemoteResult.Failure("Invalid workspace address.")
        }
        val connection = uri.toURL().openConnection() as HttpURLConnection
        try {
            connection.instanceFollowRedirects = false
            connection.connectTimeout = 10000
            connection.readTimeout = 30000
            connection.requestMethod = method
            connection.setRequestProperty("X-LiteLLM-Client", "android")
            connection.setRequestProperty("Accept", "application/json")
            token()?.let { connection.setRequestProperty("Authorization", "Bearer $it") }
            if (method == "POST") {
                connection.doOutput = true
                connection.setRequestProperty("Content-Type", "application/json")
                connection.outputStream.use { it.write(json.encodeToString(body).toByteArray(Charsets.UTF_8)) }
            }
            val code = connection.responseCode
            val stream = if (code in 200..299) connection.inputStream else connection.errorStream
            val text = stream?.bufferedReader()?.use { it.readText() }.orEmpty()
            if (code in 200..299) RemoteResult.Success(text)
            else RemoteResult.Failure(runCatching { json.decodeFromString<ErrorReply>(text).error }
                .getOrDefault("This request couldn’t be completed."), code)
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (_: Exception) {
            RemoteResult.Failure("Couldn’t reach your workspace. Check your connection and try again.")
        } finally { connection.disconnect() }
    }

    suspend fun empty(path: String, method: String = "POST", body: Map<String, String> = emptyMap()): RemoteResult<JsonObject> = request(path, method, body)

    companion object {
        fun query(path: String, name: String, value: String) = "$path?$name=${URLEncoder.encode(value, Charsets.UTF_8.name())}"
    }
}
