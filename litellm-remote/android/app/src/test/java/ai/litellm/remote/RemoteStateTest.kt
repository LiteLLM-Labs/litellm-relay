package ai.litellm.remote

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Test

class RemoteStateTest {
    private val laptop = Device("laptop", "Laptop", false, 0, "macOS")
    private val host = Device("host", "Always-on host", true, 0, "Linux")
    private val laptopSession = CodingSession("codex:one", "Laptop work", "repo", "/Users/me/repo",
        "codex", "ready", true, false, 0)
    private val hostSession = laptopSession.copy(title = "Host work", cwd = "/home/me/repo")
    @Test fun expiredApprovalNoticeSurvivesOfflineButRetryRequiresAnIdleOnlineClaudeSession() {
        val session = CodingSession("claude:one", "Task", "repo", "/repo", "claude", "ready", true, false, 1)
        val expired = ExpiredApproval("tool-1", "Bash", "Bash\n{\"command\":\"echo ok\"}")
        val ready = connected.copy(selectedSession = session, detail = SessionDetail(session, emptyList(), false, expiredApproval = expired))
        assertEquals("Approval expired because Relay restarted. The command didn't run.", EXPIRED_APPROVAL_NOTICE)
        assertEquals(expired, ready.expiredApproval)
        assertTrue(ready.canRetryExpiredApproval)
        val offline = ready.copy(devices = ready.devices.map { it.copy(online = false) })
        assertEquals(expired, offline.expiredApproval)
        assertFalse(offline.canRetryExpiredApproval)
        assertFalse(ready.copy(detailCurrent = false).canRetryExpiredApproval)
        val running = ready.detail!!.copy(session = session.copy(status = "working", canStop = true))
        assertFalse(ready.detailReceived(running).canRetryExpiredApproval)
        assertNull(ready.detailReceived(running).detailReceived(ready.detail!!).expiredApproval)
        assertNull(ready.hideExpiredApproval().detailReceived(ready.detail!!).expiredApproval)
        assertNotNull(ready.hideExpiredApproval().detailReceived(ready.detail!!.copy(expiredApproval = expired.copy(id = "tool-2"))).expiredApproval)
        assertNull(ready.copy(selectedSession = session.copy(agent = "codex")).expiredApproval)
        assertNull(ready.copy(detail = ready.detail!!.copy(expiredApproval = null)).expiredApproval)
        assertNull(ready.copy(detail = null).expiredApproval)
    }

    private val session = CodingSession("codex:one", "Task", "repo", "/repo", "codex", "working", true, true, 1)
    private val connected = RemoteState(endpoint = "https://control.example", deviceId = "host", sessionDeviceId = "host",
        devices = listOf(Device("host", "Host", true, 1, "Linux")), selectedSession = session, detailCurrent = true,
        detail = SessionDetail(session, listOf(Message("reply", "assistant", "Still here")), false,
            approvals = listOf(Approval("approval", "Run", "echo ok", true))))

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
