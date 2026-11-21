package ai.litellm.remote

import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class MarkdownParsingTest {
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
}
