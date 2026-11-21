package ai.litellm.remote

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

private val markdownParsing = Mutex()

internal suspend fun parseMarkdown(text: String, render: (String) -> CharSequence): CharSequence =
    withContext(Dispatchers.Default) { markdownParsing.withLock { render(text) } }
