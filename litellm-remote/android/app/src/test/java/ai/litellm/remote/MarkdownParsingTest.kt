package ai.litellm.remote

import androidx.compose.runtime.AbstractApplier
import androidx.compose.runtime.Composition
import androidx.compose.runtime.MonotonicFrameClock
import androidx.compose.runtime.Recomposer
import androidx.compose.runtime.SideEffect
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.snapshots.Snapshot
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Test

class MarkdownParsingTest {
    @Test fun theSameBubbleRendersEveryStreamingUpdateAndEmptyText() = runBlocking(FrameClock) {
        val reply = (1..200).map { "line $it " + "x".repeat(95) }
        val partial = "```text\n" + reply.take(142).joinToString("\n")
        val complete = "```text\n" + reply.joinToString("\n") + "\n```"
        val text = mutableStateOf(partial)
        val updates = Channel<String>(Channel.UNLIMITED)
        val otherBubble = Channel<String>(Channel.UNLIMITED)
        val caller = Thread.currentThread()
        val render: (String) -> CharSequence = {
            assertNotEquals(caller, Thread.currentThread())
            "parsed:$it"
        }
        withComposition({
            val parsed = rememberParsedMarkdown(text.value, render).value
            val other = rememberParsedMarkdown("other bubble", render).value
            SideEffect {
                updates.trySend(parsed.toString())
                otherBubble.trySend(other.toString())
            }
        }) {
            updates.awaitText("parsed:$partial")
            otherBubble.awaitText("parsed:other bubble")
            text.value = complete
            updates.awaitText("parsed:$complete")
            text.value = ""
            updates.awaitText("parsed:")
            text.value = complete
            updates.awaitText("parsed:$complete")
        }
    }

    @Test fun anOlderBlockedParseCannotReplaceNewerText() = runBlocking(FrameClock) {
        val started = CountDownLatch(1)
        val release = CountDownLatch(1)
        val finished = CountDownLatch(1)
        val text = mutableStateOf("older partial reply")
        val updates = Channel<String>(Channel.UNLIMITED)
        val render: (String) -> CharSequence = {
            if (it == "older partial reply") {
                started.countDown()
                try { check(release.await(5, TimeUnit.SECONDS)) } finally { finished.countDown() }
            }
            "parsed:$it"
        }
        try {
            withComposition({
                val parsed = rememberParsedMarkdown(text.value, render).value
                SideEffect { updates.trySend(parsed.toString()) }
            }) {
                assertTrue(withContext(Dispatchers.IO) { started.await(5, TimeUnit.SECONDS) })
                text.value = "latest complete reply"
                updates.awaitText("latest complete reply")
                release.countDown()
                withTimeout(5000) {
                    while (true) {
                        val update = updates.receive()
                        assertNotEquals("parsed:older partial reply", update)
                        if (update == "parsed:latest complete reply") break
                    }
                }
            }
        } finally {
            release.countDown()
            assertTrue(withContext(Dispatchers.IO) { finished.await(5, TimeUnit.SECONDS) })
        }
    }

    @Test fun everyBubbleIsParsedInFullOffMainOneAtATime() = runBlocking {
        val caller = Thread.currentThread()
        val active = AtomicInteger()
        val calls = AtomicInteger()
        val code = "```kotlin\n" + "val text = \"hello\"\n".repeat(10000) + "```\n😀"
        val bubbles = listOf("", "short", code, code + " streamed suffix", "short")
        val parsed = bubbles.map { text -> async {
            parseMarkdown(text) {
                assertNotEquals(caller, Thread.currentThread())
                assertEquals(1, active.incrementAndGet())
                try { calls.incrementAndGet(); it } finally { active.decrementAndGet() }
            }.toString()
        } }.awaitAll()
        assertEquals(bubbles, parsed)
        assertEquals(bubbles.size, calls.get())
    }

    @Test fun aFailedParseDoesNotPreventTheNextBubbleFromRendering() = runBlocking {
        try {
            parseMarkdown("broken") { throw IllegalArgumentException("bad markdown") }
            fail("The parse should fail")
        } catch (expected: IllegalArgumentException) {
            assertEquals("bad markdown", expected.message)
        }
        assertEquals("next", parseMarkdown("next") { it })
    }

    private suspend fun Channel<String>.awaitText(expected: String) = withTimeout(5000) {
        while (receive() != expected) Unit
    }

    private suspend fun withComposition(content: @androidx.compose.runtime.Composable () -> Unit, check: suspend () -> Unit) = coroutineScope {
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
            composition.setContent(content)
            check()
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
