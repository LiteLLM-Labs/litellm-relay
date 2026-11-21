package ai.litellm.remote

import android.app.Application
import android.content.SharedPreferences
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Proxy
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.Continuation
import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn
import kotlinx.coroutines.async
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Dispatchers
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.Assert.*
import org.junit.Test

class RefreshTest {
    @Test fun refreshAcceptsTheServerTokenReplyWithoutALoginStatus() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"new-access\",\"refreshToken\":\"new-refresh\",\"expiresAt\":123456789}"))
            val api = ApiClient(server.url("/").toString(), { throw AssertionError("Refresh must not retry authentication") }) {
                throw AssertionError("Refresh must not resolve an access bearer")
            }
            assertEquals(RemoteResult.Success(RefreshedSession("new-access", "new-refresh", 123456789)), api.refreshSession("refresh-credential"))
            val request = server.takeRequest()
            assertNull(request.getHeader("Authorization"))
            assertEquals(mapOf("refreshToken" to "refresh-credential"), api.json.decodeFromString<Map<String, String>>(request.body.readUtf8()))
        }
    }

    @Test fun emptyRefreshReplyIsAFailure() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{}"))
            assertTrue(ApiClient(server.url("/").toString()).refreshSession("refresh-credential") is RemoteResult.Failure)
        }
    }

    @Test fun temporaryRefreshFailureKeepsThePrivateSessionForRetry() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(503).setBody("{\"error\":\"Unavailable\"}"))
            val fixture = Fixture(server.url("/").toString())
            assertNull(fixture.bearer())
            assertNotNull(fixture.model.state.value.workspace)
            assertNotNull(fixture.model.state.value.selectedSession)
            assertEquals("refresh-credential", fixture.field("refreshToken"))
            assertTrue(fixture.refreshWorkspaces.isNotEmpty())
            assertEquals(1, server.requestCount)
        }
    }

    @Test fun refreshUsesItsCredentialWithoutReenteringBearerAndUnauthorizedClearsPrivateState() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(401).setBody("{\"error\":\"Sign in again.\"}"))
            val fixture = Fixture(server.url("/").toString())
            assertNull(fixture.bearer())
            assertEquals(0, fixture.bearerCalls.get())
            val request = server.takeRequest()
            assertEquals("/api/auth/refresh", request.path)
            assertEquals("POST", request.method)
            assertNull(request.getHeader("Authorization"))
            assertEquals(mapOf("refreshToken" to "refresh-credential"), fixture.api.json.decodeFromString<Map<String, String>>(request.body.readUtf8()))
            assertEquals(1, server.requestCount)
            assertNull(fixture.model.state.value.workspace)
            assertTrue(fixture.model.state.value.devices.isEmpty())
            assertNull(fixture.model.state.value.selectedSession)
            assertTrue(fixture.model.state.value.models.isEmpty())
            assertNull(fixture.field("token"))
            assertNull(fixture.field("refreshToken"))
            assertTrue(fixture.refreshWorkspaces.isEmpty())
        }
    }

    @Test fun aRotatedRefreshTokenIsNotSentAgainWhenSecureStorageRejectsIt() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}"))
            server.enqueue(MockResponse().setBody("{\"token\":\"access-3\",\"refreshToken\":\"refresh-3\",\"expiresAt\":80}"))
            val fixture = Fixture(server.url("/").toString())
            assertEquals("access-2", fixture.bearer())
            assertEquals("access-3", fixture.bearer())
            val first = fixture.api.json.decodeFromString<Map<String, String>>(server.takeRequest().body.readUtf8())
            val second = fixture.api.json.decodeFromString<Map<String, String>>(server.takeRequest().body.readUtf8())
            assertEquals(mapOf("refreshToken" to "refresh-credential"), first)
            assertEquals(mapOf("refreshToken" to "refresh-2"), second)
            assertEquals(2, server.requestCount)
        }
    }

    @Test fun simultaneousExpiredCallersShareTheRefreshFailure() = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    started.countDown()
                    release.await(5, TimeUnit.SECONDS)
                    return MockResponse().setResponseCode(401).setBody("{\"error\":\"Expired\"}")
                }
            }
            val fixture = Fixture(server.url("/").toString())
            try {
                coroutineScope {
                    val first = async { fixture.bearer() }
                    assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                    val second = async(start = CoroutineStart.UNDISPATCHED) { fixture.bearer() }
                    release.countDown()
                    assertNull(first.await())
                    assertNull(second.await())
                }
                assertEquals(1, server.requestCount)
                assertEquals(0, fixture.bearerCalls.get())
            } finally { release.countDown() }
        }
    }

    private class Fixture(endpoint: String) {
        val bearerCalls = AtomicInteger()
        var refreshWorkspaces = setOf(endpoint)
        private val editor = Proxy.newProxyInstance(SharedPreferences.Editor::class.java.classLoader, arrayOf(SharedPreferences.Editor::class.java)) { proxy, method, args ->
            when (method.name) {
                "putStringSet" -> { refreshWorkspaces = (args!![1] as Set<*>).filterIsInstance<String>().toSet(); proxy }
                "remove" -> proxy
                "apply" -> null
                else -> error("Unexpected preferences edit ${method.name}")
            }
        } as SharedPreferences.Editor
        val model = RemoteViewModel(object : Application() {
            override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
                Proxy.newProxyInstance(SharedPreferences::class.java.classLoader, arrayOf(SharedPreferences::class.java)) { _, method, args ->
                    when (method.name) {
                        "getString" -> args!![1]
                        "getStringSet" -> refreshWorkspaces
                        "edit" -> editor
                        else -> error("Unexpected preferences call ${method.name}")
                    }
                } as SharedPreferences
        })
        val api = ApiClient(endpoint) {
            if (bearerCalls.incrementAndGet() > 1) throw AssertionError("Refresh recursively re-entered bearer resolution")
            bearer()
        }

        init {
            setField("client", api)
            setField("refreshToken", "refresh-credential")
            setField("token", null)
            val session = CodingSession("codex:private", "Private", "repo", "/repo", "codex", "ready", true, false, 1)
            @Suppress("UNCHECKED_CAST")
            val state = field("mutable") as MutableStateFlow<RemoteState>
            state.value = RemoteState(endpoint = endpoint, workspace = Workspace(Identity("user", "User", 1), "https://gateway.example"),
                devices = listOf(Device("host", "Host", true, 1, "Linux")), selectedSession = session,
                models = listOf(ModelOption("private-model", "Private", true)))
        }

        fun field(name: String): Any? = RemoteViewModel::class.java.getDeclaredField(name).apply { isAccessible = true }.get(model)
        private fun setField(name: String, value: Any?) = RemoteViewModel::class.java.getDeclaredField(name).apply { isAccessible = true }.set(model, value)
        suspend fun bearer(): String? = suspendCoroutineUninterceptedOrReturn { continuation ->
            try {
                RemoteViewModel::class.java.getDeclaredMethod("bearer", Continuation::class.java).apply { isAccessible = true }.invoke(model, continuation)
            } catch (failure: InvocationTargetException) { throw failure.targetException }
        }
    }
}
