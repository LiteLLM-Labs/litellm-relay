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
                .set(model, ApiClient(endpoint, token = { null }))
            @Suppress("UNCHECKED_CAST")
            val state = RemoteViewModel::class.java.getDeclaredField("mutable").apply { isAccessible = true }
                .get(model) as MutableStateFlow<RemoteState>
            state.value = RemoteState(endpoint = endpoint, deviceId = "host",
                devices = listOf(Device("host", "Host", true, 1, "Linux")))
            try {
                sheet.launch { model.create("host", "/repo", agent, "create-once") }
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
