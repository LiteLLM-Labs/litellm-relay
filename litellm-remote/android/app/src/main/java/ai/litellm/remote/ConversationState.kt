package ai.litellm.remote

import androidx.compose.runtime.Composable
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.key
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.saveable.rememberSaveableStateHolder

@Composable
internal fun rememberConversationDraft(sessionId: String): MutableState<String> = rememberSaveable(sessionId) { mutableStateOf("") }

@Composable
internal fun ConversationStateHost(state: RemoteState, conversation: @Composable () -> Unit, other: @Composable () -> Unit) {
    key(state.conversationStateScope) {
        val holder = rememberSaveableStateHolder()
        val session = state.selectedSession
        if (state.workspace?.identity != null && session != null) {
            holder.SaveableStateProvider(arrayListOf(state.endpoint, state.sessionDeviceId, session.id)) { conversation() }
        } else other()
    }
}
