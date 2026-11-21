package ai.litellm.remote

import androidx.compose.runtime.AbstractApplier
import androidx.compose.runtime.Composition
import androidx.compose.runtime.MonotonicFrameClock
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.Recomposer
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshots.Snapshot
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test

class ConversationStateTest {
    private val claude = CodingSession("claude:one", "Claude", "repo", "/repo", "claude", "ready", true, false, 1)
    private val codex = claude.copy(id = "codex:two", title = "Codex", agent = "codex")
    private val signedIn = RemoteState(endpoint = "https://control.example", sessionDeviceId = "host", selectedSession = claude,
        workspace = Workspace(Identity("user", "User", 1), "https://gateway.example"))

    @Test fun unsentDraftsSurviveRepeatedBackAndReopenWithoutLeakingBetweenClaudeAndCodex() = runBlocking(FrameClock) {
        withEditor {
            assertEquals("", current().text)
            type("Claude draft\n😀")
            repeat(3) { index ->
                back()
                assertEquals(if (index == 0) "" else "Codex draft", open(codex).text)
                if (index == 0) type("Codex draft")
                back()
                assertEquals("Claude draft\n😀", open(claude).text)
            }
        }
    }

    @Test fun draftsAreIsolatedByDeviceAndWorkspaceEvenWhenSessionIdsMatch() = runBlocking(FrameClock) {
        withEditor {
            current()
            type("Original host")
            back()
            assertEquals("", open(claude, device = "other-host").text)
            type("Other host")
            back()
            assertEquals("", open(claude, endpoint = "https://other.example").text)
            type("Other workspace")
            back()
            assertEquals("Original host", open(claude).text)
            back()
            assertEquals("Other host", open(claude, device = "other-host").text)
            back()
            assertEquals("Other workspace", open(claude, endpoint = "https://other.example").text)
        }
    }

    @Test fun aDraftClearedAfterSendingDoesNotReturnWhenTheConversationReopens() = runBlocking(FrameClock) {
        withEditor {
            current()
            type("Sent message")
            type("")
            back()
            assertEquals("", open(claude).text)
        }
    }

    @Test fun aTransientConnectorFailureKeepsTheDraftWhileOpenAndAfterNavigation() = runBlocking(FrameClock) {
        withEditor {
            current()
            type("Unsent during reconnect")
            route.value = route.value.requestFailed(RemoteResult.Failure("Offline", 503))
            assertEquals("Unsent during reconnect", current().text)
            back()
            assertEquals("Unsent during reconnect", open(claude).text)
        }
    }

    @Test fun signingOutClearsEveryDraftEvenForTheSameAccountAndCoalescedNavigation() = runBlocking(FrameClock) {
        for (renderSignOut in listOf(true, false)) withEditor {
            current()
            type("Claude private draft")
            back()
            open(codex)
            type("Codex private draft")
            back()
            open(claude)
            type("Claude private draft after switching")
            val signedOut = route.value.requestFailed(RemoteResult.Failure("Sign in again", 401))
            route.value = signedOut
            if (renderSignOut) current()
            route.value = signedOut.copy(endpoint = signedIn.endpoint, workspace = signedIn.workspace!!.copy(identity = Identity("user", "User", 2)),
                sessionDeviceId = "host", selectedSession = claude, error = null)
            assertEquals("", current().text)
            back()
            assertEquals("", open(codex).text)
        }
    }

    private class Editor(val key: List<String?>?, val text: String, val draft: MutableState<String>?)

    private inner class Harness {
        val route = mutableStateOf(signedIn)
        val updates = Channel<Editor>(Channel.UNLIMITED)
        var editor: Editor? = null

        suspend fun current(): Editor = withTimeout(5000) {
            val expected = route.value.takeIf { it.workspace?.identity != null && it.selectedSession != null }
                ?.let { listOf(it.endpoint, it.sessionDeviceId, it.selectedSession!!.id) }
            var update = updates.receive()
            while (update.key != expected) update = updates.receive()
            update.also { editor = it }
        }

        suspend fun type(text: String) {
            editor!!.draft!!.value = text
            assertEquals(text, current().text)
        }

        suspend fun back() {
            route.value = route.value.copy(selectedSession = null, sessionDeviceId = null)
            assertNull(current().key)
        }

        suspend fun open(session: CodingSession, device: String = "host", endpoint: String = signedIn.endpoint): Editor {
            route.value = route.value.copy(endpoint = endpoint, sessionDeviceId = device, selectedSession = session)
            return current()
        }
    }

    private suspend fun withEditor(check: suspend Harness.() -> Unit) = coroutineScope {
        val harness = Harness()
        val changes = Channel<Unit>(Channel.CONFLATED)
        val observer = Snapshot.registerGlobalWriteObserver { changes.trySend(Unit) }
        val snapshots = launch { for (change in changes) Snapshot.sendApplyNotifications() }
        val recomposer = Recomposer(coroutineContext)
        val runner = launch { recomposer.runRecomposeAndApplyChanges() }
        val composition = Composition(object : AbstractApplier<Unit>(Unit) {
            override fun insertTopDown(index: Int, instance: Unit) = Unit
            override fun insertBottomUp(index: Int, instance: Unit) = Unit
            override fun remove(index: Int, count: Int) = Unit
            override fun move(from: Int, to: Int, count: Int) = Unit
            override fun onClear() = Unit
        }, recomposer)
        try {
            composition.setContent {
                val state = harness.route.value
                ConversationStateHost(state, conversation = {
                    val session = state.selectedSession!!
                    val draft = rememberConversationDraft(session.id)
                    val text = draft.value
                    SideEffect { harness.updates.trySend(Editor(listOf(state.endpoint, state.sessionDeviceId, session.id), text, draft)) }
                }) { SideEffect { harness.updates.trySend(Editor(null, "", null)) } }
            }
            harness.check()
        } finally {
            composition.dispose()
            recomposer.cancel()
            runner.cancel()
            snapshots.cancel()
            observer.dispose()
        }
    }

    private object FrameClock : MonotonicFrameClock {
        override suspend fun <R> withFrameNanos(onFrame: (Long) -> R): R = onFrame(System.nanoTime())
    }
}
