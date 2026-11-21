package ai.litellm.remote

import android.app.Application
import android.content.SharedPreferences
import androidx.lifecycle.ViewModelStore
import java.lang.reflect.Proxy
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.Dispatcher
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.RecordedRequest
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class RemoteViewModelTest {
    @Before fun setUp() { Dispatchers.setMain(Dispatchers.Unconfined) }
    @After fun tearDown() { Dispatchers.resetMain() }

    @Test fun retrySendsOneNewUserTurnAndHidesNoticeEvenIfSendingFails() = runBlocking {
        val session = CodingSession("claude:one", "Task", "repo", "/repo", "claude", "ready", true, false, 1)
        val expired = ExpiredApproval("tool-1", "Bash", "Bash\necho ok")
        val detail = SessionDetail(session, emptyList(), false, expiredApproval = expired)
        val model = RemoteViewModel(object : Application() {
            override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
                Proxy.newProxyInstance(SharedPreferences::class.java.classLoader, arrayOf(SharedPreferences::class.java)) { _, method, args ->
                    if (method.name == "getString") args!![1] else error("Unexpected preferences call ${method.name}")
                } as SharedPreferences
        })
        val store = ViewModelStore().apply { put("remote", model) }
        @Suppress("UNCHECKED_CAST")
        val state = RemoteViewModel::class.java.getDeclaredField("mutable").apply { isAccessible = true }.get(model) as MutableStateFlow<RemoteState>
        try { MockWebServer().use { server ->
            val endpoint = server.url("/").toString().trimEnd('/')
            RemoteViewModel::class.java.getDeclaredField("client").apply { isAccessible = true }.set(model, ApiClient(endpoint) { null })
            val ready = RemoteState(endpoint = endpoint, selectedSession = session, sessionDeviceId = "host", detail = detail,
                detailCurrent = true, devices = listOf(Device("host", "Host", true, 1, "Linux")))
            state.value = ready.copy(devices = ready.devices.map { it.copy(online = false) })
            assertFalse(model.retryExpiredApproval("offline"))
            assertEquals(0, server.requestCount)
            assertNotNull(model.state.value.expiredApproval)
            state.value = ready.copy(detail = detail.copy(session = session.copy(status = "working", canStop = true)))
            assertFalse(model.retryExpiredApproval("running"))
            assertEquals(0, server.requestCount)
            for (success in listOf(true, false)) {
                state.value = ready
                server.enqueue(MockResponse().setResponseCode(if (success) 200 else 422).setBody(if (success) "{}" else "{\"error\":\"Cannot send\"}"))
                if (success) server.enqueue(MockResponse().setBody(Json.encodeToString(detail)))
                assertEquals(success, model.retryExpiredApproval("retry-once"))
                val request = server.takeRequest(5, TimeUnit.SECONDS)!!
                assertEquals("POST", request.method)
                assertEquals("/api/devices/host/sessions/claude:one/messages", request.path)
                assertEquals(Json.parseToJsonElement("""{"text":"Relay restarted before I could approve your last command, so it didn't run. Please try it again.","requestId":"retry-once"}"""), Json.parseToJsonElement(request.body.readUtf8()))
                if (success) assertEquals("GET", server.takeRequest(5, TimeUnit.SECONDS)!!.method)
                assertNull(model.state.value.expiredApproval)
                assertFalse(model.retryExpiredApproval("duplicate"))
                if (!success) assertNotNull(model.state.value.actionError)
            }
            assertEquals(3, server.requestCount)
        } } finally { store.clear() }
    }

    @Test fun createdClaudeModelsLoadAfterTheNewSessionSheetIsCancelled() = runBlocking {
        modelsLoadAfterSheetCancellation("claude")
    }

    @Test fun createdCodexModelsLoadAfterTheNewSessionSheetIsCancelled() = runBlocking {
        modelsLoadAfterSheetCancellation("codex")
    }

    private suspend fun modelsLoadAfterSheetCancellation(agent: String) {
        val session = CodingSession("$agent:new", "New", "repo", "/repo", agent, "ready", true, false, 1)
        val models = (1..4).map { ModelOption("model-$it", "Model $it", it == 1) }
        val detailStarted = CountDownLatch(1)
        val releaseDetail = CountDownLatch(1)
        val sheet = CoroutineScope(SupervisorJob() + Dispatchers.Unconfined)
        val store = ViewModelStore()
        MockWebServer().use { server ->
            server.dispatcher = object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse = when {
                    request.method == "POST" -> MockResponse().setBody(Json.encodeToString(session))
                    request.path!!.endsWith("/models") -> MockResponse().setBody(Json.encodeToString(ModelPage(models)))
                    else -> {
                        detailStarted.countDown()
                        releaseDetail.await(5, TimeUnit.SECONDS)
                        MockResponse().setBody(Json.encodeToString(SessionDetail(session, emptyList(), false)))
                    }
                }
            }
            val model = RemoteViewModel(object : Application() {
                override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences =
                    Proxy.newProxyInstance(SharedPreferences::class.java.classLoader, arrayOf(SharedPreferences::class.java)) { _, method, args ->
                        if (method.name == "getString") args!![1] else error("Unexpected preferences call ${method.name}")
                    } as SharedPreferences
            })
            store.put("remote", model)
            val endpoint = server.url("/").toString().trimEnd('/')
            RemoteViewModel::class.java.getDeclaredField("client").apply { isAccessible = true }
                .set(model, ApiClient(endpoint) { null })
            @Suppress("UNCHECKED_CAST")
            val state = RemoteViewModel::class.java.getDeclaredField("mutable").apply { isAccessible = true }
                .get(model) as MutableStateFlow<RemoteState>
            state.value = RemoteState(endpoint = endpoint, deviceId = "host")
            try {
                sheet.launch { model.create("/repo", agent, "create-once") }
                withTimeout(5000) { model.state.first { it.selectedSession != null } }
                assertTrue(withContext(Dispatchers.IO) { detailStarted.await(5, TimeUnit.SECONDS) })
                sheet.cancel()
                releaseDetail.countDown()
                val loaded = withTimeout(5000) { model.state.first { it.models.size == models.size && it.detail != null } }
                assertEquals(models, loaded.models)
                assertEquals(session, loaded.detail!!.session)
                assertFalse(loaded.busy)
            } finally {
                releaseDetail.countDown()
                sheet.cancel()
                store.clear()
            }
        }
    }
}
