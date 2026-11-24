package ai.litellm.remote

import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class RemoteStateTest {
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
    private val laptop = Device("laptop", "Laptop", false, 0, "macOS")
    private val host = Device("host", "Always-on host", true, 0, "Linux")
    private val laptopSession = CodingSession("codex:one", "Laptop work", "repo", "/Users/me/repo",
        "codex", "ready", true, false, 0)
    private val hostSession = laptopSession.copy(title = "Host work", cwd = "/home/me/repo")

    @Test fun aRunningClaudeConversationStaysVisibleWhileItsOldChildIsRediscoveredAfterRestart() {
        val claude = session.copy(id = "claude:one", agent = "claude")
        val running = connected.copy(selectedSession = claude, detail = connected.detail!!.copy(session = claude))
        val external = SessionDetail(claude.copy(status = "handoff_required", canSend = false, canStop = false), emptyList(), false,
            error = "Finish this turn, run /exit, then run relay remote handoff.")
        var reconnecting = running.requestFailed(RemoteResult.Failure("Device offline", 503)).detailReceived(external)
        repeat(2) {
            assertEquals(running.detail!!.messages, reconnecting.detail!!.messages)
            assertNotEquals("handoff_required", reconnecting.detail!!.session.status)
            assertFalse(reconnecting.hasCurrentDetail)
            assertFalse(reconnecting.detail!!.session.canSend)
            assertFalse(reconnecting.detail!!.session.canStop)
            assertTrue(reconnecting.detail!!.approvals.isEmpty())
            reconnecting = reconnecting.detailReceived(external)
        }
        val ready = external.copy(session = claude.copy(status = "ready", canSend = true, canStop = false), messages = running.detail!!.messages, error = null)
        val recovered = reconnecting.detailReceived(ready)
        assertEquals(ready, recovered.detail)
        assertTrue(recovered.hasCurrentDetail)
        assertNull(recovered.error)
        val idle = running.copy(detail = ready)
        assertEquals(external, idle.detailReceived(external).detail)
        assertEquals(external, running.copy(detail = null).detailReceived(external).detail)
        val another = external.copy(session = external.session.copy(id = "claude:another"))
        assertEquals(another, running.detailReceived(another).detail)
        val codex = external.copy(session = external.session.copy(id = session.id, agent = "codex"))
        assertEquals(codex, connected.detailReceived(codex).detail)
    }

    @Test fun unavailableDevicesAndFailedRequestsDisableStaleControlsWithoutLosingHistory() {
        assertTrue(connected.hasCurrentDetail)
        for (status in listOf<Int?>(503, 504, null)) {
            val failed = connected.requestFailed(RemoteResult.Failure("Disconnected", status))
            assertFalse(failed.hasCurrentDetail)
            assertEquals(connected.detail, failed.detail)
            assertFalse(failed.busy)
            assertFalse(failed.loading)
            assertFalse(failed.copy(error = null).hasCurrentDetail)
        }
        assertFalse(connected.copy(devices = connected.devices.map { it.copy(online = false) }).hasCurrentDetail)
        assertFalse(connected.copy(detail = null).hasCurrentDetail)
        assertFalse(connected.copy(devices = emptyList()).hasCurrentDetail)
        assertTrue(connected.requestFailed(RemoteResult.Failure("Offline", 503)).copy(error = null, detailCurrent = true).hasCurrentDetail)
    }

    @Test fun aPersistentClaudeHandoffBecomesVisibleAfterTheReconnectGracePeriod() {
        val claude = session.copy(id = "claude:one", agent = "claude")
        val running = connected.copy(selectedSession = claude, detail = connected.detail!!.copy(session = claude))
        val external = SessionDetail(claude.copy(status = "handoff_required", canSend = false, canStop = false), emptyList(), false,
            error = "Finish this turn, run /exit, then run relay remote handoff.")
        val first = running.detailReceived(external, now = 1_000)
        val stillRunning = first.detailReceived(external, now = 91_000)
        assertTrue(stillRunning.sessionReconnecting)
        assertEquals(running.detail!!.messages, stillRunning.detail!!.messages)
        assertFalse(stillRunning.hasCurrentDetail)
        val offline = stillRunning.requestFailed(RemoteResult.Failure("Offline", 503))
        val expired = offline.detailReceived(external, now = 181_000)
        assertFalse(expired.sessionReconnecting)
        assertEquals(external, expired.detail)
        assertFalse(expired.detail!!.session.canSend)
        assertFalse(expired.detail!!.session.canStop)
        val later = expired.detailReceived(external, now = 300_000)
        assertFalse(later.sessionReconnecting)
        assertEquals(external, later.detail)
        val ready = external.copy(session = claude.copy(status = "ready", canSend = true, canStop = false), error = null)
        val recovered = stillRunning.detailReceived(ready, now = 92_000)
        assertFalse(recovered.sessionReconnecting)
        assertEquals(ready, recovered.detail)
        assertTrue(recovered.hasCurrentDetail)
        val anotherTurn = expired.detailReceived(running.detail!!, now = 301_000).detailReceived(external, now = 302_000)
        assertTrue(anotherTurn.sessionReconnecting)
        assertEquals(running.detail!!.messages, anotherTurn.detail!!.messages)
    }

    @Test fun expiredAuthenticationClearsPrivateWorkspaceState() {
        val expired = connected.copy(gateway = "https://gateway.example").requestFailed(RemoteResult.Failure("Sign in again", 401))
        assertNull(expired.selectedSession)
        assertNull(expired.detail)
        assertTrue(expired.devices.isEmpty())
        assertEquals("https://gateway.example", expired.gateway)
    }

    @Test fun repliesFromAnotherDeviceOrWorkspaceCannotReplaceTheConversation() {
        assertTrue(connected.acceptsReply(connected.endpoint, "host", session.id))
        assertFalse(connected.acceptsReply("https://other.example", "host", session.id))
        assertFalse(connected.acceptsReply(connected.endpoint, "laptop", session.id))
        assertFalse(connected.acceptsReply(connected.endpoint, "host", "codex:two"))
        assertFalse(connected.copy(selectedSession = null).acceptsReply(connected.endpoint, "host", session.id))
    }

    @Test fun olderPollsCannotOverwriteNewerResultsOrSurviveNavigation() {
        val replies = ReplyGate()
        val first = replies.next()
        val second = replies.next()
        assertFalse(replies.accepts(first))
        assertTrue(replies.accepts(second))
        replies.next()
        assertFalse(replies.accepts(second))
    }

    @Test fun actionErrorsStayInTheConversationAndOnlyDisconnectsInvalidateDetail() {
        for (status in listOf<Int?>(409, 422, 429, 503, 504, null)) {
            val failed = connected.actionFailed(RemoteResult.Failure("Try again", status))
            assertEquals("Try again", failed.actionError)
            assertNull(failed.error)
            assertEquals(status in listOf(409, 422, 429), failed.hasCurrentDetail)
            assertEquals(connected.detail, failed.detail)
        }
        assertNull(connected.actionFailed(RemoteResult.Failure("Expired", 401)).workspace)
        assertNull(connected.actionFailed(RemoteResult.Failure("Expired", 401)).detail)
    }

    @Test fun selectedConversationIsPolledWhileNotCurrentAndControlsRecoverWithoutDismissal() = runBlocking {
        for (status in listOf<Int?>(503, 504, null)) {
            var current = connected.actionFailed(RemoteResult.Failure("Offline", status))
            var polls = 0
            assertFalse(current.hasCurrentDetail)
            current.refreshSelection({
                polls++
                current = current.requestFailed(RemoteResult.Failure("Still offline", status))
            }, { fail("The selected conversation must be polled") })
            assertFalse(current.hasCurrentDetail)
            current.refreshSelection({
                polls++
                current = current.detailReceived(connected.detail!!)
            }, { fail("The selected conversation must be polled") })
            assertEquals(2, polls)
            assertTrue(current.hasCurrentDetail)
            assertNull(current.error)
        }
        var sessionPolls = 0
        RemoteState().refreshSelection({ fail("No conversation selected") }, { sessionPolls++ })
        assertEquals(1, sessionPolls)
    }

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
