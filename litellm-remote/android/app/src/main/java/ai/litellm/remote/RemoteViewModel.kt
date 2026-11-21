package ai.litellm.remote

import android.app.Application
import android.content.Context
import android.os.SystemClock
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.net.URI
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

private const val CLAUDE_RECONNECT_GRACE_MS = 180_000L

data class RemoteState(
    val gateway: String = "", val controlAddress: String = "", val endpoint: String = "",
    val workspace: Workspace? = null, val devices: List<Device> = emptyList(), val deviceId: String? = null,
    val sessions: List<CodingSession> = emptyList(), val nextCursor: String? = null,
    val login: LoginStart? = null, val teams: List<Team> = emptyList(),
    val selectedSession: CodingSession? = null, val sessionDeviceId: String? = null, val detail: SessionDetail? = null,
    val busy: Boolean = false, val loading: Boolean = false, val error: String? = null,
    val models: List<ModelOption> = emptyList(), val modelError: String? = null, val actionError: String? = null,
    val detailCurrent: Boolean = false,
    val dismissedExpiredApproval: String? = null,
    val sessionReconnectStartedAt: Long? = null,
) {
    val sessionReconnecting get() = sessionReconnectStartedAt != null
    val device get() = devices.firstOrNull { it.id == deviceId }
    val hasCurrentDetail get() = detailCurrent && detail != null && error == null && devices.firstOrNull { it.id == sessionDeviceId }?.online == true
    val turnRunning get() = (detail?.session ?: selectedSession)?.let { it.canStop || it.status in setOf("working", "needs_approval") } == true
    val expiredApproval get() = detail?.expiredApproval?.takeIf { selectedSession?.agent == "claude" && !turnRunning && it.id != dismissedExpiredApproval }
    val canRetryExpiredApproval get() = expiredApproval != null && hasCurrentDetail && !turnRunning && detail?.session?.canSend == true

    fun hideExpiredApproval() = copy(dismissedExpiredApproval = detail?.expiredApproval?.id ?: dismissedExpiredApproval)

    fun acceptsReply(endpoint: String, device: String, session: String?) =
        this.endpoint == endpoint && sessionDeviceId == device && selectedSession?.id == session

    fun requestFailed(failure: RemoteResult.Failure): RemoteState = if (failure.status == 401) {
        RemoteState(gateway = gateway, controlAddress = controlAddress, error = failure.message)
    } else copy(error = failure.message, detailCurrent = detailCurrent && failure.status !in listOf(null, 503, 504), busy = false, loading = false)

    fun actionFailed(failure: RemoteResult.Failure): RemoteState = if (failure.status == 401) requestFailed(failure)
    else copy(actionError = failure.message, detailCurrent = detailCurrent && failure.status !in listOf(null, 503, 504))

    fun detailReceived(value: SessionDetail, now: Long = SystemClock.elapsedRealtime()): RemoteState {
        val previous = detail
        if (value.session.agent == "claude" && value.session.status == "handoff_required" && previous?.session?.id == value.session.id &&
            (sessionReconnecting || previous.session.canStop || previous.session.status == "working")) {
            val started = sessionReconnectStartedAt ?: now
            if (now - started < CLAUDE_RECONNECT_GRACE_MS) {
                return copy(detail = previous.copy(session = previous.session.copy(canSend = false, canStop = false), approvals = emptyList(), error = null),
                    detailCurrent = false, error = null, sessionReconnectStartedAt = started)
            }
        }
        val updated = copy(detail = value, detailCurrent = true, error = null, sessionReconnectStartedAt = null)
        return if (value.session.canStop || value.session.status == "working") updated.copy(dismissedExpiredApproval = detail?.expiredApproval?.id ?: dismissedExpiredApproval) else updated
    }

    suspend fun refreshSelection(loadDetail: suspend () -> Unit, refreshSessions: suspend () -> Unit) {
        if (selectedSession != null) loadDetail() else refreshSessions()
    }
}

class RemoteViewModel @JvmOverloads constructor(application: Application, private val vault: SessionStorage = SessionVault(application)) : AndroidViewModel(application) {
    private val preferences = application.getSharedPreferences("remote.settings", Context.MODE_PRIVATE)
    private val mutable = MutableStateFlow(RemoteState(gateway = preferences.getString("gateway", "").orEmpty(),
        controlAddress = preferences.getString("controlAddress", "").orEmpty()))
    val state = mutable.asStateFlow()
    private var client: ApiClient? = null
    private var token: String? = null
    private var refreshToken: String? = null
    private var accessExpiresAt: Long = 0
    private val refreshMutex = Mutex()
    private val detailReplies = ReplyGate()
    private val sessionReplies = ReplyGate()
    private val deviceReplies = ReplyGate()

    init { if (state.value.gateway.isNotEmpty()) connect() }

    fun gateway(value: String) { mutable.update { it.copy(gateway = value) } }
    fun controlAddress(value: String) { mutable.update { it.copy(controlAddress = value) } }
    fun dismissError() { mutable.update { it.copy(error = null) } }

    fun connect() { viewModelScope.launch {
        if (state.value.busy) return@launch
        val gateway = workspaceAddress(state.value.gateway, gateway = true)
        if (gateway == null) { fail("Enter your LiteLLM gateway’s HTTPS address."); return@launch }
        mutable.update { it.copy(busy = true, error = null) }
        val override = state.value.controlAddress
        val endpoint = if (override.isNotBlank()) workspaceAddress(override) else {
            when (val manifest = ApiClient(gateway) { null }.request<Discovery>("${URI(gateway).path}/.well-known/litellm-remote.json")) {
                is RemoteResult.Success -> manifest.value.takeIf { workspaceAddress(it.gatewayUrl, true) == gateway }
                    ?.controlUrl?.let { workspaceAddress(it) }?.takeIf { it.startsWith("https://") }
                is RemoteResult.Failure -> null
            }
        }
        if (endpoint == null) { fail("Remote control isn’t configured at this gateway yet. Add its service address under Advanced."); return@launch }
        val connection = refreshMutex.withLock {
            client?.takeIf { it.endpoint == endpoint } ?: run {
                val saved = vault.load(endpoint)
                if (endpoint in refreshWorkspaces()) {
                    refreshToken = saved
                    token = null
                    accessExpiresAt = 0
                } else {
                    refreshToken = null
                    token = saved
                }
                lateinit var api: ApiClient
                api = ApiClient(endpoint, { refreshAccess(api) }) { bearer(api) }
                api.also { client = it }
            }
        }
        val reply = connection.request<Workspace>("/api/me")
        if (client !== connection) return@launch
        when (reply) {
            is RemoteResult.Success -> {
                if (workspaceAddress(reply.value.gateway, true) != gateway) { fail("This remote service is connected to a different gateway."); return@launch }
                client = connection
                preferences.edit().putString("gateway", gateway).putString("controlAddress", override).apply()
                if (reply.value.identity == null) {
                    if (refreshToken != null) { fail("Couldn’t refresh your sign-in. Check your connection and try again."); return@launch }
                    vault.remove(endpoint); forgetRefresh(endpoint); token = null
                }
                mutable.update { RemoteState(gateway = it.gateway, controlAddress = override, workspace = reply.value, endpoint = endpoint) }
                if (reply.value.identity != null) refresh()
            }
            is RemoteResult.Failure -> fail(reply.message)
        }
    } }

    fun startLogin() { viewModelScope.launch {
        val api = client ?: return@launch
        mutable.update { it.copy(busy = true, error = null) }
        when (val result = api.request<LoginStart>("/api/auth/start", "POST")) {
            is RemoteResult.Success -> {
                val gateway = URI(workspaceAddress(state.value.gateway, true) ?: return@launch)
                val verification = runCatching { URI(result.value.verificationUrl) }.getOrNull()
                if (verification?.scheme != gateway.scheme || verification.host != gateway.host || verification.port != gateway.port) {
                    fail("The sign-in page does not belong to your gateway."); return@launch
                }
                mutable.update { it.copy(login = result.value, busy = false) }
            }
            is RemoteResult.Failure -> fail(result.message)
        }
    } }

    suspend fun pollLogin(team: String? = null) {
        val login = state.value.login ?: return
        val api = client ?: return
        when (val result = api.request<LoginPoll>("/api/auth/poll", "POST", mapOf("flowToken" to login.flowToken) + (team?.let { mapOf("team" to it) } ?: emptyMap()))) {
            is RemoteResult.Success -> {
                val value = result.value
                if (value.status == "teams") { mutable.update { it.copy(teams = value.teams) }; return }
                val access = value.token ?: return
                if (value.refreshToken != null) {
                    if (!vault.save(state.value.endpoint, value.refreshToken)) { fail("Couldn’t securely save your sign-in."); return }
                    rememberRefresh(state.value.endpoint)
                    refreshToken = value.refreshToken
                    token = access
                    accessExpiresAt = value.identity?.expiresAt ?: value.expiresAt ?: 0L
                } else {
                    if (!vault.save(state.value.endpoint, access)) { fail("Couldn’t securely save your sign-in."); return }
                    forgetRefresh(state.value.endpoint)
                    refreshToken = null
                    token = access
                }
                val workspace = api.request<Workspace>("/api/me")
                if (client !== api) return
                when (workspace) {
                    is RemoteResult.Success -> { mutable.update { it.copy(workspace = workspace.value, login = null, teams = emptyList(), error = null) }; refresh() }
                    is RemoteResult.Failure -> handle(workspace)
                }
            }
            is RemoteResult.Failure -> {
                if (result.status == 410) mutable.update { it.copy(login = null) }
                fail(result.message)
            }
        }
    }

    suspend fun refresh() {
        val api = client ?: return
        val ticket = deviceReplies.next()
        when (val result = api.request<Devices>("/api/devices")) {
            is RemoteResult.Success -> if (client === api && deviceReplies.accepts(ticket)) mutable.update { previous ->
                val selected = previous.deviceId?.takeIf { id -> result.value.devices.any { it.id == id } }
                    ?: result.value.devices.firstOrNull { it.online }?.id ?: result.value.devices.firstOrNull()?.id
                previous.copy(devices = result.value.devices, deviceId = selected,
                    sessions = if (selected == previous.deviceId) previous.sessions else emptyList())
            }
            is RemoteResult.Failure -> { if (client === api && deviceReplies.accepts(ticket)) handle(result); return }
        }
        state.value.refreshSelection({ loadDetail() }, { refreshSessions() })
    }

    fun selectDevice(id: String) { viewModelScope.launch {
        mutable.update { it.copy(deviceId = id, sessions = emptyList(), nextCursor = null, error = null) }
        refreshSessions()
    } }

    suspend fun refreshSessions(older: Boolean = false) {
        val api = client ?: return
        val id = state.value.deviceId ?: return
        val ticket = sessionReplies.next()
        val base = "/api/devices/$id/sessions"
        val path = if (older) state.value.nextCursor?.let { ApiClient.query(base, "cursor", it) } ?: base else base
        mutable.update { it.copy(loading = it.sessions.isEmpty()) }
        try { when (val result = api.request<SessionPage>(path)) {
            is RemoteResult.Success -> if (client === api && sessionReplies.accepts(ticket) && state.value.deviceId == id) mutable.update {
                it.copy(sessions = if (older) (it.sessions + result.value.sessions).distinctBy { session -> session.id } else result.value.sessions,
                    nextCursor = result.value.nextCursor, loading = false, error = result.value.warnings.takeIf { warnings -> warnings.isNotEmpty() }?.joinToString("\n"))
            }
            is RemoteResult.Failure -> if (client === api && sessionReplies.accepts(ticket) && state.value.deviceId == id) handle(result)
        } } finally {
            if (sessionReplies.accepts(ticket)) mutable.update { it.copy(loading = false) }
        }
    }

    fun openSession(session: CodingSession) { openSession(session, state.value.deviceId) }

    private fun openSession(session: CodingSession, device: String?) { viewModelScope.launch {
        detailReplies.next()
        mutable.update { it.copy(selectedSession = session, sessionDeviceId = device, detail = null, models = emptyList(), modelError = null, error = null, actionError = null, sessionReconnectStartedAt = null) }
        loadDetail()
        loadModels()
    } }
    fun closeSession() { detailReplies.next(); mutable.update { it.copy(selectedSession = null, sessionDeviceId = null, detail = null, models = emptyList(), modelError = null, error = null, sessionReconnectStartedAt = null) } }

    private suspend fun loadDetail() {
        val api = client ?: return
        val session = state.value.selectedSession ?: return
        val device = state.value.sessionDeviceId ?: return
        val ticket = detailReplies.next()
        when (val result = api.request<SessionDetail>("/api/devices/$device/sessions/${session.id}")) {
            is RemoteResult.Success -> mutable.update { if (client === api && detailReplies.accepts(ticket) && it.acceptsReply(api.endpoint, device, session.id)) it.detailReceived(result.value) else it }
            is RemoteResult.Failure -> if (client === api && detailReplies.accepts(ticket) && state.value.acceptsReply(api.endpoint, device, session.id)) handle(result)
        }
    }

    suspend fun create(cwd: String, agent: String, requestId: String): Boolean {
        val api = client ?: return false
        val device = state.value.deviceId ?: return false
        mutable.update { it.copy(busy = true, error = null) }
        return try { when (val result = api.request<CodingSession>("/api/devices/$device/sessions", "POST", mapOf("cwd" to cwd, "agent" to agent, "requestId" to requestId))) {
            is RemoteResult.Success -> {
                openSession(result.value, device)
                true
            }
            is RemoteResult.Failure -> { handle(result); false }
        } } finally { mutable.update { it.copy(busy = false) } }
    }

    suspend fun loadModels() {
        val api = client ?: return
        val device = state.value.sessionDeviceId ?: return
        val session = state.value.selectedSession?.id ?: return
        var cursor: String? = null
        val loaded = mutableListOf<ModelOption>()
        do {
            val base = "/api/devices/$device/models"
            when (val page = api.request<ModelPage>(cursor?.let { ApiClient.query(base, "cursor", it) } ?: base)) {
                is RemoteResult.Success -> { loaded.addAll(page.value.models); cursor = page.value.nextCursor }
                is RemoteResult.Failure -> {
                    if (state.value.acceptsReply(api.endpoint, device, session)) {
                        if (page.status == 401) handle(page) else mutable.update { it.copy(modelError = page.message) }
                    }
                    return
                }
            }
        } while (cursor != null)
        mutable.update { if (it.acceptsReply(api.endpoint, device, session)) it.copy(models = loaded.distinctBy { option -> option.id }, modelError = null) else it }
    }

    suspend fun send(text: String, requestId: String, model: String? = null): Boolean {
        if (!state.value.hasCurrentDetail) return false
        val api = client ?: return false
        val session = state.value.selectedSession ?: return false
        val device = state.value.sessionDeviceId ?: return false
        mutable.update { it.copy(actionError = null) }
        val input = mapOf("text" to text, "requestId" to requestId) + (model?.let { mapOf("model" to it) } ?: emptyMap())
        return when (val result = api.empty("/api/devices/$device/sessions/${session.id}/messages", body = input)) {
            is RemoteResult.Success -> {
                mutable.update { if (it.acceptsReply(api.endpoint, device, session.id)) it.hideExpiredApproval() else it }
                loadDetail(); true
            }
            is RemoteResult.Failure -> { if (client === api && state.value.acceptsReply(api.endpoint, device, session.id)) handleAction(result); false }
        }
    }

    suspend fun retryExpiredApproval(requestId: String): Boolean {
        if (!state.value.canRetryExpiredApproval) return false
        mutable.update { it.hideExpiredApproval() }
        return send(EXPIRED_APPROVAL_RETRY, requestId)
    }

    suspend fun interrupt(requestId: String): Boolean {
        if (!state.value.hasCurrentDetail) return false
        val api = client ?: return false
        val session = state.value.selectedSession ?: return false
        val device = state.value.sessionDeviceId ?: return false
        return when (val result = api.empty("/api/devices/$device/sessions/${session.id}/interrupt", body = mapOf("requestId" to requestId))) {
            is RemoteResult.Success -> { loadDetail(); true }
            is RemoteResult.Failure -> { if (client === api && state.value.acceptsReply(api.endpoint, device, session.id)) handleAction(result); false }
        }
    }

    suspend fun approve(approval: Approval, decision: String) {
        if (!state.value.hasCurrentDetail) return
        val api = client ?: return
        val session = state.value.selectedSession ?: return
        val device = state.value.sessionDeviceId ?: return
        when (val result = api.empty("/api/devices/$device/sessions/${session.id}/approvals/${approval.id}", body = mapOf("decision" to decision))) {
            is RemoteResult.Success -> loadDetail()
            is RemoteResult.Failure -> if (client === api && state.value.acceptsReply(api.endpoint, device, session.id)) handleAction(result)
        }
    }

    fun removeDevice(id: String) { viewModelScope.launch {
        when (val result = client?.empty("/api/devices/$id", "DELETE")) {
            is RemoteResult.Success -> refresh()
            is RemoteResult.Failure -> handle(result)
            null -> Unit
        }
    } }

    fun signOut() { viewModelScope.launch {
        when (val result = client?.empty("/api/auth/logout")) {
            is RemoteResult.Success -> disconnect()
            is RemoteResult.Failure -> if (result.status == 401) disconnect() else fail("Couldn’t revoke this sign-in. Reconnect and try again.")
            null -> disconnect()
        }
    } }

    fun disconnect() {
        detailReplies.next()
        sessionReplies.next()
        deviceReplies.next()
        val endpoint = client?.endpoint ?: state.value.endpoint
        forgetRefresh(endpoint)
        vault.remove(endpoint)
        token = null
        refreshToken = null
        accessExpiresAt = 0
        client = null
        mutable.update { RemoteState(gateway = it.gateway, controlAddress = it.controlAddress) }
    }
    private fun refreshWorkspaces() = preferences.getStringSet("refreshWorkspaces", emptySet())?.toMutableSet() ?: mutableSetOf()

    private fun rememberRefresh(endpoint: String) {
        preferences.edit().putStringSet("refreshWorkspaces", refreshWorkspaces().apply { add(endpoint) }).apply()
    }

    private fun forgetRefresh(endpoint: String) {
        preferences.edit().putStringSet("refreshWorkspaces", refreshWorkspaces().apply { remove(endpoint) }).apply()
    }

    private suspend fun bearer(api: ApiClient): String? {
        val decision = refreshMutex.withLock {
            if (client !== api) PhoneBearer.SignedOut else phoneBearer(token, accessExpiresAt, refreshToken != null, System.currentTimeMillis())
        }
        return when (decision) {
            is PhoneBearer.Current -> decision.token
            PhoneBearer.Refresh -> if (refreshAccess(api)) refreshMutex.withLock { token.takeIf { client === api } } else null
            PhoneBearer.SignedOut -> null
        }
    }

    private suspend fun refreshAccess(api: ApiClient): Boolean {
        val refresh = refreshToken ?: return false
        return withContext(NonCancellable + Dispatchers.IO) { refreshMutex.withLock {
            if (client !== api) return@withLock false
            if (refreshToken != refresh) return@withLock token != null
            when (val result = api.refreshSession(refresh)) {
                is RemoteResult.Success -> {
                    if (client !== api || refreshToken != refresh) return@withLock false
                    val next = result.value.refreshToken
                    if (!vault.save(api.endpoint, next)) {
                        disconnect()
                        fail("Couldn’t securely save your sign-in. Please sign in again.")
                        return@withLock false
                    }
                    refreshToken = next
                    token = result.value.token
                    accessExpiresAt = result.value.expiresAt
                    true
                }
                is RemoteResult.Failure -> {
                    if (client === api && refreshToken == refresh && result.status == 401) disconnect()
                    false
                }
            }
        } }
    }

    private fun fail(message: String) { mutable.update { it.copy(error = message, busy = false, loading = false) } }
    private fun handle(failure: RemoteResult.Failure) {
        if (failure.status == 401) disconnect()
        mutable.update { it.requestFailed(failure) }
    }
    private fun handleAction(failure: RemoteResult.Failure) {
        if (failure.status == 401) disconnect()
        mutable.update { it.actionFailed(failure) }
    }
}
