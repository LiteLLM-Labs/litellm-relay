package ai.litellm.remote

import android.text.SpannableString
import android.text.Spanned
import android.text.style.URLSpan
import androidx.compose.runtime.Composable
import androidx.compose.runtime.State
import androidx.compose.runtime.produceState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext

private val markdownParsing = Mutex()

@Composable
internal fun rememberParsedMarkdown(text: String, render: (String) -> CharSequence): State<CharSequence> =
    produceState<CharSequence>(initialValue = text, key1 = text, key2 = render) {
        value = text
        value = parseMarkdown(text, render)
    }

internal suspend fun parseMarkdown(text: String, render: (String) -> CharSequence): CharSequence =
    withContext(Dispatchers.Default) {
        markdownParsing.withLock {
            val rendered = render(text)
            if (rendered !is Spanned) return@withLock rendered
            val copy = SpannableString(rendered)
            copy.getSpans(0, copy.length, URLSpan::class.java).forEach { span ->
                if (!span.url.startsWith("https://", ignoreCase = true)) copy.removeSpan(span)
            }
            copy
        }
    }
