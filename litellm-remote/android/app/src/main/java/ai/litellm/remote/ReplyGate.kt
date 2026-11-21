package ai.litellm.remote

class ReplyGate {
    private var current = 0L
    fun next(): Long = ++current
    fun accepts(ticket: Long): Boolean = ticket == current
}
