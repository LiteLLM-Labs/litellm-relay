package ai.litellm.remote

import android.app.Application
import android.content.SharedPreferences
import androidx.lifecycle.viewModelScope
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Proxy
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.Continuation
import kotlin.coroutines.intrinsics.suspendCoroutineUninterceptedOrReturn
import kotlinx.coroutines.async
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withTimeout
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import okhttp3.mockwebserver.SocketPolicy
import org.junit.Assert.*
import org.junit.Test
import org.junit.Before
import org.junit.After

@OptIn(ExperimentalCoroutinesApi::class)
class RefreshTest {
    @Before fun setUp() { Dispatchers.setMain(Dispatchers.Unconfined) }
    @After fun tearDown() { Dispatchers.resetMain() }

    @Test fun freshLoginUsesIdentityExpiryBeforeMakingItsFirstAuthenticatedCall() = runBlocking {
        val future = System.currentTimeMillis() + 3_600_000
        for (expiry in listOf("\"identity\":{\"subject\":\"user\",\"label\":\"User\",\"expiresAt\":$future},\"expiresAt\":0", "\"expiresAt\":$future", "\"identity\":null")) {
            MockWebServer().use { server ->
                val paths = java.util.Collections.synchronizedList(mutableListOf<String>())
                server.dispatcher = object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse {
                        paths.add(request.path!!)
                        return MockResponse().setBody(when (request.path) {
                            "/api/auth/poll" -> "{\"status\":\"ready\",\"token\":\"access-1\",\"refreshToken\":\"refresh-1\",$expiry}"
                            "/api/auth/refresh" -> "{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":$future}"
                            "/api/me" -> "{\"gateway\":\"https://gateway.example\",\"identity\":{\"subject\":\"user\",\"label\":\"User\",\"expiresAt\":$future}}"
                            "/api/devices" -> "{\"devices\":[]}"
                            else -> error("Unexpected login request ${request.path}")
                        })
                    }
                }
                val fixture = Fixture(server.url("/").toString())
                fixture.setField("refreshToken", null)
                fixture.state.value = RemoteState(endpoint = fixture.endpoint, login = LoginStart("flow", "code", "https://gateway.example/login"))
                fixture.model.pollLogin()
                val needsRefresh = expiry == "\"identity\":null"
                assertEquals(if (needsRefresh) 1 else 0, paths.count { it == "/api/auth/refresh" })
                server.takeRequest()
                if (needsRefresh) server.takeRequest()
                val firstAuthenticated = server.takeRequest()
                assertEquals("/api/me", firstAuthenticated.path)
                assertEquals("Bearer access-${if (needsRefresh) 2 else 1}", firstAuthenticated.getHeader("Authorization"))
                assertEquals(if (needsRefresh) "refresh-2" else "refresh-1", fixture.vault.load(fixture.endpoint))
            }
        }
    }

    @Test fun failedRotationSaveSignsOutVisiblyAndCannotReplayTheOldTokenAfterAColdStart() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}"))
            server.enqueue(MockResponse().setBody("{\"gateway\":\"https://gateway.example\"}"))
            server.enqueue(MockResponse().setBody("{\"gateway\":\"https://gateway.example\"}"))
            val fixture = Fixture(server.url("/").toString())
            fixture.vault.rejectSave = true
            fixture.connect()
            assertNull(fixture.model.state.value.workspace)
            assertTrue(fixture.model.state.value.error!!.contains("securely"))
            assertNull(fixture.vault.load(fixture.endpoint))
            val cold = Fixture(fixture.endpoint, fixture.vault)
            assertNull(cold.bearer())
            cold.connect()
            assertNull(cold.model.state.value.workspace!!.identity)
            assertEquals(3, server.requestCount)
            assertEquals("/api/auth/refresh", server.takeRequest().path)
            repeat(2) {
                val request = server.takeRequest()
                assertEquals("/api/me", request.path)
                assertNull(request.getHeader("Authorization"))
            }
        }
    }

    @Test fun successfulRotationDoesNotRequireDeletingTheOldCredentialFirst() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}"))
            val fixture = Fixture(server.url("/").toString())
            fixture.vault.rejectRemove = true
            assertEquals("access-2", fixture.bearer())
            assertEquals("refresh-2", fixture.vault.load(fixture.endpoint))
            assertEquals(1, server.requestCount)
            assertNull(fixture.model.state.value.error)
        }
    }

    @Test fun transientRefreshFailurePreservesTheSavedCredentialForAColdStart() = runBlocking {
        for (status in listOf<Int?>(503, 500, null)) {
            MockWebServer().use { server ->
                val rotations = AtomicInteger()
                val bodies = java.util.Collections.synchronizedList(mutableListOf<String>())
                val future = System.currentTimeMillis() + 3_600_000
                server.dispatcher = object : Dispatcher() {
                    override fun dispatch(request: RecordedRequest): MockResponse = when (request.path) {
                        "/api/auth/refresh" -> {
                            bodies.add(request.body.readUtf8())
                            if (rotations.getAndIncrement() == 0) {
                                if (status != null) MockResponse().setResponseCode(status).setBody("{\"error\":\"Unavailable\"}")
                                else MockResponse().setBody("x".repeat(100)).setSocketPolicy(SocketPolicy.DISCONNECT_DURING_RESPONSE_BODY)
                            } else MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":$future}")
                        }
                        "/api/me" -> if (request.getHeader("Authorization") == "Bearer access-2") {
                            MockResponse().setBody("{\"gateway\":\"https://gateway.example\",\"identity\":{\"subject\":\"user\",\"label\":\"User\",\"expiresAt\":$future}}")
                        } else MockResponse().setResponseCode(401).setBody("{\"error\":\"Sign in again\"}")
                        "/api/devices" -> MockResponse().setBody("{\"devices\":[]}")
                        else -> MockResponse().setResponseCode(404)
                    }
                }
                val fixture = Fixture(server.url("/").toString())
                assertNull(fixture.bearer())
                assertEquals("refresh-credential", fixture.vault.load(fixture.endpoint))
                val cold = Fixture(fixture.endpoint, fixture.vault)
                cold.connect()
                assertEquals("user", cold.model.state.value.workspace?.identity?.subject)
                assertEquals("refresh-2", cold.vault.load(cold.endpoint))
                assertEquals(listOf("{\"refreshToken\":\"refresh-credential\"}", "{\"refreshToken\":\"refresh-credential\"}"), bodies)
            }
        }
    }

    @Test fun failedRotationSaveAndRemovalStillSignOutWithAVisibleError() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}"))
            val fixture = Fixture(server.url("/").toString())
            fixture.vault.rejectSave = true
            fixture.vault.rejectRemove = true
            assertNull(fixture.bearer())
            assertEquals(1, server.requestCount)
            assertNull(fixture.field("token"))
            assertNull(fixture.field("refreshToken"))
            assertNull(fixture.model.state.value.workspace)
            assertTrue(fixture.refreshWorkspaces.isEmpty())
            assertTrue(fixture.model.state.value.error!!.contains("securely"))
        }
    }

    @Test fun cancellingTheCallerStillPersistsASuccessfulRotation() = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        MockWebServer().use { server ->
            val credentials = java.util.Collections.synchronizedList(mutableListOf<String>())
            val future = System.currentTimeMillis() + 3_600_000
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    credentials.add(request.body.readUtf8())
                    started.countDown()
                    release.await(5, TimeUnit.SECONDS)
                    return MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":$future}")
                }
            }
            val fixture = Fixture(server.url("/").toString())
            try {
                val caller = async { fixture.bearer() }
                assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                assertEquals("refresh-credential", fixture.vault.load(fixture.endpoint))
                caller.cancel()
                release.countDown()
                caller.join()
                assertEquals("refresh-2", fixture.vault.load(fixture.endpoint))
                assertEquals("access-2", fixture.bearer())
                val cold = Fixture(fixture.endpoint, fixture.vault)
                assertEquals("access-2", cold.bearer())
                assertEquals(listOf("{\"refreshToken\":\"refresh-credential\"}", "{\"refreshToken\":\"refresh-2\"}"), credentials)
            } finally { release.countDown() }
        }
    }

    @Test fun reconnectDuringRotationKeepsTheNewCredentialAndDoesNotRotateAgain() = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        MockWebServer().use { server ->
            val rotations = AtomicInteger()
            val future = System.currentTimeMillis() + 3_600_000
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = MockResponse().setBody(when (request.path) {
                    "/api/auth/refresh" -> {
                        rotations.incrementAndGet()
                        started.countDown()
                        release.await(5, TimeUnit.SECONDS)
                        "{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":$future}"
                    }
                    "/api/me" -> "{\"gateway\":\"https://gateway.example\",\"identity\":{\"subject\":\"user\",\"label\":\"User\",\"expiresAt\":$future}}"
                    "/api/devices" -> "{\"devices\":[]}"
                    else -> error("Unexpected reconnect request ${request.path}")
                })
            }
            val fixture = Fixture(server.url("/").toString())
            try {
                val caller = async { fixture.bearer() }
                assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                fixture.model.gateway("https://gateway.example")
                fixture.model.controlAddress(fixture.endpoint)
                fixture.model.connect()
                release.countDown()
                assertEquals("access-2", caller.await())
                withTimeout(5000) { fixture.model.state.first { !it.busy && it.workspace?.identity != null } }
                assertEquals("refresh-2", fixture.vault.load(fixture.endpoint))
                assertEquals("access-2", fixture.bearer())
                assertEquals(1, rotations.get())
            } finally { release.countDown() }
        }
    }

    @Test fun concurrentExpiredCallersShareOneDurableRotation() = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        MockWebServer().use { server ->
            val future = System.currentTimeMillis() + 3_600_000
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    started.countDown()
                    release.await(5, TimeUnit.SECONDS)
                    return MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":$future}")
                }
            }
            val fixture = Fixture(server.url("/").toString())
            try {
                val first = async { fixture.bearer() }
                assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                val second = async(start = CoroutineStart.UNDISPATCHED) { fixture.bearer() }
                release.countDown()
                assertEquals("access-2", first.await())
                assertEquals("access-2", second.await())
                assertEquals("refresh-2", fixture.vault.load(fixture.endpoint))
                assertEquals(1, server.requestCount)
            } finally { release.countDown() }
        }
    }

    @Test fun explicitDisconnectDuringRotationDoesNotRestorePrivateCredentials() = runBlocking {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse {
                    started.countDown()
                    release.await(5, TimeUnit.SECONDS)
                    return MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}")
                }
            }
            val fixture = Fixture(server.url("/").toString())
            try {
                val caller = async { fixture.bearer() }
                assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                fixture.model.disconnect()
                release.countDown()
                assertNull(caller.await())
                assertNull(fixture.vault.load(fixture.endpoint))
                assertNull(fixture.field("refreshToken"))
                assertNull(fixture.model.state.value.workspace)
            } finally { release.countDown() }
        }
    }

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
            assertNull(fixture.vault.load(fixture.endpoint))
        }
    }

    @Test fun successfulRotationsPersistAndNeverReuseThePreviousRefreshToken() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"token\":\"access-2\",\"refreshToken\":\"refresh-2\",\"expiresAt\":50}"))
            server.enqueue(MockResponse().setBody("{\"token\":\"access-3\",\"refreshToken\":\"refresh-3\",\"expiresAt\":80}"))
            val fixture = Fixture(server.url("/").toString())
            assertEquals("access-2", fixture.bearer())
            assertEquals("access-3", fixture.bearer())
            assertEquals("refresh-3", fixture.vault.load(fixture.endpoint))
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

    private class FakeVault(endpoint: String) : SessionStorage {
        private val saved = ConcurrentHashMap(mapOf(endpoint.trimEnd('/') to "refresh-credential"))
        var rejectSave = false
        var rejectRemove = false
        override fun load(workspace: String) = saved[workspace]
        override fun save(workspace: String, token: String): Boolean {
            if (rejectSave) return false
            saved[workspace] = token
            return true
        }
        override fun remove(workspace: String): Boolean {
            if (rejectRemove) return false
            saved.remove(workspace)
            return true
        }
    }

    private class Fixture(address: String, val vault: FakeVault = FakeVault(address)) {
        val endpoint = address.trimEnd('/')
        val bearerCalls = AtomicInteger()
        var refreshWorkspaces = setOf(endpoint)
        private val editor = Proxy.newProxyInstance(SharedPreferences.Editor::class.java.classLoader, arrayOf(SharedPreferences.Editor::class.java)) { proxy, method, args ->
            when (method.name) {
                "putStringSet" -> { refreshWorkspaces = (args!![1] as Set<*>).filterIsInstance<String>().toSet(); proxy }
                "remove", "putString" -> proxy
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
        }, vault)
        val api = ApiClient(endpoint) {
            if (bearerCalls.incrementAndGet() > 20) throw AssertionError("Refresh recursively re-entered bearer resolution")
            bearer()
        }
        @Suppress("UNCHECKED_CAST")
        val state get() = field("mutable") as MutableStateFlow<RemoteState>

        init {
            setField("client", api)
            setField("refreshToken", vault.load(endpoint))
            setField("token", null)
            val session = CodingSession("codex:private", "Private", "repo", "/repo", "codex", "ready", true, false, 1)
            @Suppress("UNCHECKED_CAST")
            val state = field("mutable") as MutableStateFlow<RemoteState>
            state.value = RemoteState(endpoint = endpoint, workspace = Workspace(Identity("user", "User", 1), "https://gateway.example"),
                devices = listOf(Device("host", "Host", true, 1, "Linux")), selectedSession = session,
                models = listOf(ModelOption("private-model", "Private", true)))
        }

        fun field(name: String): Any? = RemoteViewModel::class.java.getDeclaredField(name).apply { isAccessible = true }.get(model)
        fun setField(name: String, value: Any?) = RemoteViewModel::class.java.getDeclaredField(name).apply { isAccessible = true }.set(model, value)
        suspend fun connect() {
            model.gateway("https://gateway.example")
            model.controlAddress(endpoint)
            model.connect()
            withTimeout(5000) { model.viewModelScope.coroutineContext[Job]!!.children.toList().joinAll() }
        }
        suspend fun bearer(): String? {
            val api = field("client") as? ApiClient ?: return null
            return suspendCoroutineUninterceptedOrReturn { continuation ->
                try {
                    RemoteViewModel::class.java.getDeclaredMethod("bearer", ApiClient::class.java, Continuation::class.java).apply { isAccessible = true }.invoke(model, api, continuation)
                } catch (failure: InvocationTargetException) { throw failure.targetException }
            }
        }
    }
}
