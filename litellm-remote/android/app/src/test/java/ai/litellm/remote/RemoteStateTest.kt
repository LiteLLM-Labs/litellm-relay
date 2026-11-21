package ai.litellm.remote

import org.junit.Assert.*
import org.junit.Test

class RemoteStateTest {
    private val session = CodingSession("codex:one", "Task", "repo", "/repo", "codex", "working", true, true, 1)
    private val connected = RemoteState(endpoint = "https://control.example", deviceId = "host", sessionDeviceId = "host",
        devices = listOf(Device("host", "Host", true, 1, "Linux")), selectedSession = session, detailCurrent = true,
        detail = SessionDetail(session, listOf(Message("reply", "assistant", "Still here")), false,
            approvals = listOf(Approval("approval", "Run", "echo ok", true))))

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

    @Test fun longRepliesRemainCompleteInBoundedPagesIncludingStreamingSuffixes() {
        val text = "code and markdown\n".repeat(10000)
        val pages = replyPages(text)
        assertEquals(text, pages.joinToString(""))
        assertTrue(pages.all { it.length <= 8000 })
        assertEquals(listOf(""), replyPages(""))
        assertEquals(listOf("short"), replyPages("short"))
        assertEquals(text + "new streamed text", replyPages(text + "new streamed text").joinToString(""))
    }
}
