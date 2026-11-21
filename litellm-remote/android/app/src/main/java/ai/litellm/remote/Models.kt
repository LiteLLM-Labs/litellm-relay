package ai.litellm.remote

import kotlinx.serialization.Serializable

@Serializable data class Identity(val subject: String, val label: String, val expiresAt: Long)
@Serializable data class Workspace(val identity: Identity? = null, val gateway: String)
@Serializable data class Discovery(val gatewayUrl: String, val controlUrl: String)
@Serializable data class Device(val id: String, val name: String, val online: Boolean, val lastSeen: Long, val platform: String)
@Serializable data class Devices(val devices: List<Device>)
@Serializable data class CodingSession(
    val id: String, val title: String, val project: String, val cwd: String, val agent: String,
    val status: String, val canSend: Boolean, val canStop: Boolean, val updatedAt: Long, val model: String? = null,
) {
    val agentName get() = if (agent == "claude") "Claude Code" else "Codex"
    val needsAttention get() = status in setOf("needs_approval", "needs_input", "handoff_required")
    val activity get() = when (status) {
        "working" -> "Working"
        "needs_approval" -> "Needs approval"
        "needs_input" -> "Needs input"
        "handoff_required" -> "Set up on Mac"
        "ready" -> "Ready"
        "error" -> "Needs attention"
        else -> "Saved"
    }
}
@Serializable data class SessionPage(val sessions: List<CodingSession>, val nextCursor: String? = null, val warnings: List<String> = emptyList())
@Serializable data class ModelOption(val id: String, val label: String, val isDefault: Boolean)
@Serializable data class ModelPage(val models: List<ModelOption>, val nextCursor: String? = null)
@Serializable data class Message(val id: String, val role: String, val text: String)
@Serializable data class Approval(val id: String, val title: String, val text: String, val canAccept: Boolean)
const val EXPIRED_APPROVAL_NOTICE = "Approval expired because the connector restarted. The command didn't run."
const val EXPIRED_APPROVAL_RETRY = "The connector restarted before I could approve your last command, so it didn't run. Please try it again."
@Serializable data class ExpiredApproval(val id: String, val tool: String, val summary: String)
@Serializable data class SessionDetail(
    val session: CodingSession, val messages: List<Message>, val olderMessages: Boolean,
    val error: String? = null, val approvals: List<Approval> = emptyList(),
    val expiredApproval: ExpiredApproval? = null,
)
@Serializable data class LoginStart(val flowToken: String, val code: String, val verificationUrl: String)
@Serializable data class Team(val id: String, val name: String)
@Serializable data class LoginPoll(
    val status: String,
    val token: String? = null,
    val refreshToken: String? = null,
    val expiresAt: Long? = null,
    val teams: List<Team> = emptyList(),
)

internal sealed class PhoneBearer {
    data class Current(val token: String) : PhoneBearer()
    data object Refresh : PhoneBearer()
    data object SignedOut : PhoneBearer()
}

internal fun phoneBearer(access: String?, expiresAt: Long, hasRefresh: Boolean, now: Long): PhoneBearer {
    if (access != null && (!hasRefresh || now + 180_000 < expiresAt)) return PhoneBearer.Current(access)
    if (hasRefresh) return PhoneBearer.Refresh
    return PhoneBearer.SignedOut
}
@Serializable data class ErrorReply(val error: String)

sealed interface RemoteResult<out T> {
    data class Success<T>(val value: T) : RemoteResult<T>
    data class Failure(val message: String, val status: Int? = null) : RemoteResult<Nothing>
}
