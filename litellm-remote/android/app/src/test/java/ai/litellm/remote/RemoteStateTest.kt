package ai.litellm.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class RemoteStateTest {
    private val laptop = Device("laptop", "Laptop", false, 0, "macOS")
    private val host = Device("host", "Always-on host", true, 0, "Linux")
    private val laptopSession = CodingSession("codex:one", "Laptop work", "repo", "/Users/me/repo",
        "codex", "ready", true, false, 0)
    private val hostSession = laptopSession.copy(title = "Host work", cwd = "/home/me/repo")

    @Test fun offlineLaptopKeepsItsSessionsWhileOnlyCreationFallsBack() {
        val state = RemoteState(devices = listOf(laptop, host), deviceId = laptop.id,
            sessionDeviceId = laptop.id, selectedSession = laptopSession,
            sessionPages = mapOf(laptop.id to SessionPage(listOf(laptopSession), "older", listOf("Cannot reach Codex.")),
                host.id to SessionPage(listOf(hostSession))))

        assertEquals(host, state.newSessionDevice)
        assertEquals(laptop, state.device)
        assertEquals(laptop, state.sessionDevice)
        assertEquals(listOf(laptopSession), state.sessions)
        assertEquals("older", state.nextCursor)
        assertNull(state.sessionListWarning)
        assertEquals("Cannot reach Codex.", state.copy(devices = listOf(laptop.copy(online = true), host)).sessionListWarning)
        assertEquals(listOf(hostSession), state.copy(deviceId = host.id).sessions)
        assertEquals(laptop, state.copy(deviceId = host.id).sessionDevice)
    }

    @Test fun onlineSelectionWinsAndAnotherMacIsNotAnAlwaysOnFallback() {
        val onlineLaptop = laptop.copy(online = true)
        assertEquals(onlineLaptop, RemoteState(devices = listOf(onlineLaptop, host), deviceId = laptop.id).newSessionDevice)
        assertNull(RemoteState(devices = listOf(laptop, onlineLaptop.copy(id = "other"), host.copy(online = false)),
            deviceId = laptop.id).newSessionDevice)
        assertNull(RemoteState(devices = listOf(host, host.copy(id = "offline-host", online = false)),
            deviceId = "offline-host").newSessionDevice)
    }
}
