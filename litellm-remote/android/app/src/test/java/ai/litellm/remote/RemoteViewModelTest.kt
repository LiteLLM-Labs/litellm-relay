package ai.litellm.remote

import android.app.Application
import android.content.SharedPreferences
import androidx.lifecycle.ViewModelStore
import java.lang.reflect.Proxy
import java.util.concurrent.TimeUnit
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class RemoteViewModelTest {
    @Test fun cachedSessionsSurviveNewViewModelAndAreRemovedOnDisconnect() = runBlocking {
        val disk = mutableMapOf<String, MutableMap<String, String>>()
        fun newModel() = RemoteViewModel(object : Application() {
            override fun getSharedPreferences(name: String?, mode: Int): SharedPreferences {
                val rows = disk.getOrPut(requireNotNull(name)) { mutableMapOf() }
                val editor = Proxy.newProxyInstance(SharedPreferences.Editor::class.java.classLoader,
                    arrayOf(SharedPreferences.Editor::class.java)) { proxy, method, args ->
                    when (method.name) {
                        "putString" -> { rows[args!![0] as String] = args[1] as String; proxy }
                        "remove" -> { rows.remove(args!![0]); proxy }
                        "apply" -> null
                        else -> error("Unexpected editor call ${method.name}")
                    }
                }
                return Proxy.newProxyInstance(SharedPreferences::class.java.classLoader, arrayOf(SharedPreferences::class.java)) { _, method, args ->
                    when (method.name) {
                        "getString" -> rows[args!![0]] ?: args[1]
                        "edit" -> editor
                        else -> error("Unexpected preferences call ${method.name}")
                    }
                } as SharedPreferences
            }
        })
        val session = CodingSession("claude:one", "Mac work", "repo", "/repo", "claude", "ready", true, false, 1)
        val fresh = session.copy(id = "claude:two", title = "New Mac session")
        val hostSession = session.copy(title = "Linux work")
        val laptop = Device("mac", "Mac", true, 1, "macOS")
        val host = Device("host", "Linux", true, 1, "Linux")
        val workspace = Workspace(Identity("account", "Account", 1), "https://gateway.example")
        MockWebServer().use { server ->
            val endpoint = server.url("/").toString().trimEnd('/')
            val clientField = RemoteViewModel::class.java.getDeclaredField("client").apply { isAccessible = true }
            fun restored(value: Workspace = workspace) = newModel().apply {
                restoreWorkspace(endpoint, value)
                clientField.set(this, ApiClient(endpoint) { null })
            }
            val warm = restored()
            server.enqueue(MockResponse().setBody(Json.encodeToString(Devices(listOf(laptop, host)))))
            server.enqueue(MockResponse().setBody(Json.encodeToString(SessionPage(listOf(session)))))
            warm.refresh()
            warm.rememberSession(laptop.id, fresh)
            warm.rememberSession(host.id, hostSession)

            val cold = restored()
            assertEquals(laptop.copy(online = false), cold.state.value.device)
            assertEquals(listOf(fresh, session), cold.state.value.sessions)
            assertEquals(listOf(hostSession), cold.state.value.sessionsFor(host.id))
            assertNull(cold.state.value.newSessionDevice)
            server.enqueue(MockResponse().setBody(Json.encodeToString(Devices(listOf(laptop.copy(online = false), host)))))
            cold.refresh()
            assertEquals(host, cold.state.value.newSessionDevice)
            assertEquals(listOf(fresh, session), cold.state.value.sessions)
            assertEquals(3, server.requestCount)
            assertTrue(restored(workspace.copy(identity = workspace.identity!!.copy(subject = "another"))).state.value.sessions.isEmpty())

            val liveSession = session.copy(title = "New title from device")
            server.enqueue(MockResponse().setBody(Json.encodeToString(Devices(listOf(laptop)))))
            server.enqueue(MockResponse().setBody(Json.encodeToString(SessionPage(listOf(liveSession)))))
            cold.refresh()
            assertEquals(listOf(liveSession), cold.state.value.sessions)
            assertTrue(restored().state.value.sessionsFor(host.id).isEmpty())
            cold.disconnect()
            assertNull(cold.state.value.workspace)
            assertTrue(restored().state.value.devices.isEmpty())
        }
    }

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

}
