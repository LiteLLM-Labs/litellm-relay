package ai.litellm.remote

fun replyPages(text: String): List<String> = text.chunked(8000).ifEmpty { listOf("") }
