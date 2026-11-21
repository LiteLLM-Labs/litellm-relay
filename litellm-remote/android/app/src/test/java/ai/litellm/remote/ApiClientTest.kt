package ai.litellm.remote

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class ApiClientTest {
    @Test fun workspaceAddressesRejectCredentialsAndPlaintextRemoteHosts() {
        assertNull(workspaceAddress("https://user:password@example.com"))
        assertNull(workspaceAddress("http://192.168.1.5:4174"))
        assertNull(workspaceAddress("https://example.com?redirect=another-host"))
        assertNull(workspaceAddress("https://example.com/api/sessions"))
        assertEquals("https://gateway.example.com", workspaceAddress("gateway.example.com/v1/", true))
        assertEquals("https://gateway.example.com/proxy", workspaceAddress("https://gateway.example.com/proxy/v1", true))
        assertNotNull(workspaceAddress("http://127.0.0.1:4174"))
    }

    @Test fun messagesCarryNativeAuthenticationAndJsonWithoutChangingTheText() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setBody("{\"turnId\":\"accepted\"}").addHeader("Content-Type", "application/json"))
            val client = ApiClient(server.url("/").toString()) { "opaque-session" }
            val text = "Continue with \"quotes\"\nAnd a new line & Unicode λ"
            val result = client.empty("/api/devices/device/sessions/session/messages", body = mapOf("text" to text, "requestId" to "request", "model" to "gateway/chosen-model"))
            assertTrue(result is RemoteResult.Success)
            val received = server.takeRequest()
            assertEquals("Bearer opaque-session", received.getHeader("Authorization"))
            assertEquals("android", received.getHeader("X-LiteLLM-Client"))
            val input = client.json.decodeFromString<Map<String, String>>(received.body.readUtf8())
            assertEquals(text, input["text"])
            assertEquals("gateway/chosen-model", input["model"])
        }
    }

    @Test fun handoffAndApprovalSessionsShowTheRightControls() {
        val page = Json.decodeFromString<SessionPage>("""{"sessions":[
            {"id":"claude:session","agent":"claude","title":"Task","project":"repo","cwd":"/repo","status":"handoff_required","canSend":false,"canStop":false,"updatedAt":1},
            {"id":"codex:session","agent":"codex","title":"Task","project":"repo","cwd":"/repo","status":"needs_approval","canSend":true,"canStop":true,"updatedAt":1}
        ]}""")
        assertEquals("Set up on device", page.sessions[0].activity)
        assertFalse(page.sessions[0].canSend)
        assertTrue(page.sessions[0].needsAttention)
        assertEquals("Needs approval", page.sessions[1].activity)
        assertTrue(page.sessions[1].canStop)
        assertNotEquals(page.sessions[0].agentName, page.sessions[1].agentName)
    }

    @Test fun permissionFailuresKeepTheServerMessageAndStatus() = runBlocking {
        MockWebServer().use { server ->
            server.enqueue(MockResponse().setResponseCode(403).setBody("{\"error\":\"This device is not yours.\"}"))
            val result = ApiClient(server.url("/").toString()) { "session" }.empty("/api/devices/device/sessions")
            assertEquals(RemoteResult.Failure("This device is not yours.", 403), result)
        }
    }
}
