package ai.litellm.remote

import android.app.Application
import android.content.Context
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import java.net.URI
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class RemoteState(
    val gateway: String = "", val controlAddress: String = "", val endpoint: String = "",
    val workspace: Workspace? = null, val devices: List<Device> = emptyList(), val deviceId: String? = null,
    val sessionPages: Map<String, SessionPage> = emptyMap(),
    val login: LoginStart? = null, val teams: List<Team> = emptyList(),
    val selectedSession: CodingSession? = null, val sessionDeviceId: String? = null, val detail: SessionDetail? = null,
    val busy: Boolean = false, val loading: Boolean = false, val error: String? = null,
    val models: List<ModelOption> = emptyList(), val modelError: String? = null, val actionError: String? = null,
    val detailCurrent: Boolean = false,
    val dismissedExpiredApproval: String? = null,
) {
    val device get() = devices.firstOrNull { it.id == deviceId }
    val hasCurrentDetail get() = detailCurrent && detail != null && error == null && devices.firstOrNull { it.id == sessionDeviceId }?.online == true
    val turnRunning get() = (detail?.session ?: selectedSession)?.let { it.canStop || it.status in setOf("working", "needs_approval") } == true
    val expiredApproval get() = detail?.expiredApproval?.takeIf { selectedSession?.agent == "claude" && !turnRunning && it.id != dismissedExpiredApproval }
    val canRetryExpiredApproval get() = expiredApproval != null && hasCurrentDetail && !turnRunning && detail?.session?.canSend == true

    fun hideExpiredApproval() = copy(dismissedExpiredApproval = detail?.expiredApproval?.id ?: dismissedExpiredApproval)

    fun acceptsReply(endpoint: String, device: String, session: String?) =
        this.endpoint == endpoint && sessionDeviceId == device && selectedSession?.id == session

    fun detailReceived(value: SessionDetail): RemoteState {
        val updated = copy(detail = value, detailCurrent = true, error = null)
        return if (value.session.canStop || value.session.status == "working") updated.copy(dismissedExpiredApproval = detail?.expiredApproval?.id ?: dismissedExpiredApproval) else updated
    }

    private val selectedPage get() = deviceId?.let { sessionPages[it] }
    val sessions get() = selectedPage?.sessions.orEmpty()
    val nextCursor get() = selectedPage?.nextCursor
    val sessionListWarning get() = selectedPage?.warnings.orEmpty()
        .takeIf { device?.online == true && it.isNotEmpty() }?.joinToString("\n")
    val sessionDevice get() = devices.firstOrNull { it.id == sessionDeviceId }
    val newSessionDevice get() = device?.let { selected ->
        when {
            selected.online -> selected
            selected.platform == "macOS" -> devices.firstOrNull { it.online && it.platform == "Linux" }
            else -> null
        }
    }
}

class RemoteViewModel(application: Application) : AndroidViewModel(application) {
    private val preferences = application.getSharedPreferences("remote.settings", Context.MODE_PRIVATE)
    private val vault = SessionVault(application)
    private val mutable = MutableStateFlow(RemoteState(gateway = preferences.getString("gateway", "").orEmpty(),
        controlAddress = preferences.getString("controlAddress", "").orEmpty()))
    val state = mutable.asStateFlow()
    private var client: ApiClient? = null
    private var token: String? = null

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
        token = vault.load(endpoint)
        val connection = ApiClient(endpoint) { token }
        when (val reply = connection.request<Workspace>("/api/me")) {
            is RemoteResult.Success -> {
                if (workspaceAddress(reply.value.gateway, true) != gateway) { fail("This remote service is connected to a different gateway."); return@launch }
                client = connection
                preferences.edit().putString("gateway", gateway).putString("controlAddress", override).apply()
                mutable.update { it.copy(workspace = reply.value, endpoint = endpoint, busy = false) }
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
                if (value.status != "ready" || value.token == null) return
                if (!vault.save(state.value.endpoint, value.token)) { fail("Couldn’t securely save your sign-in."); return }
                token = value.token
                when (val workspace = api.request<Workspace>("/api/me")) {
                    is RemoteResult.Success -> { mutable.update { it.copy(workspace = workspace.value, login = null, teams = emptyList(), error = null) }; refresh() }
                    is RemoteResult.Failure -> fail(workspace.message)
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
        val result = api.request<Devices>("/api/devices")
        if (client !== api) return
        val sessionWasOnline = state.value.sessionDevice?.online == true
        when (result) {
            is RemoteResult.Success -> mutable.update { previous ->
                val selected = previous.deviceId?.takeIf { id -> result.value.devices.any { it.id == id } }
                    ?: result.value.devices.firstOrNull { it.online }?.id ?: result.value.devices.firstOrNull()?.id
                val ids = result.value.devices.map { it.id }.toSet()
                previous.copy(devices = result.value.devices, deviceId = selected,
                    sessionPages = previous.sessionPages.filterKeys { it in ids },
                    loading = previous.loading && result.value.devices.any { it.id == selected && it.online })
            }
            is RemoteResult.Failure -> { handle(result); return }
        }
        if (state.value.selectedSession != null) {
            loadDetail()
            if (!sessionWasOnline && state.value.sessionDevice?.online == true) loadModels()
        } else refreshSessions()
    }

    fun selectDevice(id: String) { viewModelScope.launch {
        mutable.update { it.copy(deviceId = id, loading = false, error = null) }
        refreshSessions()
    } }

    suspend fun refreshSessions(older: Boolean = false) {
        val api = client ?: return
        val id = state.value.device?.takeIf { it.online }?.id ?: return
        val base = "/api/devices/$id/sessions"
        val path = if (older) state.value.nextCursor?.let { ApiClient.query(base, "cursor", it) } ?: base else base
        mutable.update { it.copy(loading = it.sessions.isEmpty()) }
        val result = api.request<SessionPage>(path)
        if (client !== api || state.value.devices.none { it.id == id }) return
        when (result) {
            is RemoteResult.Success -> mutable.update {
                val page = if (older) result.value.copy(
                    sessions = (it.sessionPages[id]?.sessions.orEmpty() + result.value.sessions).distinctBy { session -> session.id }
                ) else result.value
                val updated = it.copy(sessionPages = it.sessionPages + (id to page))
                if (it.deviceId == id) updated.copy(loading = false, error = null)
                else updated
            }
            is RemoteResult.Failure -> if (state.value.deviceId == id) handle(result)
        }
    }

    fun openSession(device: String, session: CodingSession) { viewModelScope.launch {
        mutable.update { it.copy(selectedSession = session, sessionDeviceId = device, detail = null, error = null, actionError = null, models = emptyList(), modelError = null) }
        loadDetail()
        loadModels()
    } }
    fun closeSession() { mutable.update { it.copy(selectedSession = null, sessionDeviceId = null, detail = null, error = null) } }

    private suspend fun loadDetail() {
        val api = client ?: return
        val session = state.value.selectedSession ?: return
        val device = state.value.sessionDevice?.takeIf { it.online }?.id ?: return
        when (val result = api.request<SessionDetail>("/api/devices/$device/sessions/${session.id}")) {
            is RemoteResult.Success -> if (state.value.sessionDeviceId == device && state.value.selectedSession?.id == session.id)
                mutable.update { it.detailReceived(result.value) }
            is RemoteResult.Failure -> if (state.value.sessionDeviceId == device && state.value.selectedSession?.id == session.id) handle(result)
        }
    }

    suspend fun create(device: String, cwd: String, agent: String, requestId: String): Boolean {
        val api = client ?: return false
        if (state.value.devices.none { it.id == device && it.online }) return false
        mutable.update { it.copy(busy = true, error = null) }
        return when (val result = api.request<CodingSession>("/api/devices/$device/sessions", "POST", mapOf("cwd" to cwd, "agent" to agent, "requestId" to requestId))) {
            is RemoteResult.Success -> {
                mutable.update { it.copy(selectedSession = result.value, sessionDeviceId = device, detail = null, busy = false,
                    models = emptyList(), modelError = null, actionError = null) }
                viewModelScope.launch { loadDetail(); loadModels() }
                true
            }
            is RemoteResult.Failure -> { handle(result); false }
        }
    }

    suspend fun loadModels() {
        val api = client ?: return
        val device = state.value.sessionDevice?.takeIf { it.online }?.id ?: return
        var cursor: String? = null
        val loaded = mutableListOf<ModelOption>()
        do {
            val base = "/api/devices/$device/models"
            when (val page = api.request<ModelPage>(cursor?.let { ApiClient.query(base, "cursor", it) } ?: base)) {
                is RemoteResult.Success -> { loaded.addAll(page.value.models); cursor = page.value.nextCursor }
                is RemoteResult.Failure -> {
                    if (device == state.value.sessionDeviceId) mutable.update { it.copy(modelError = page.message) }
                    return
                }
            }
        } while (cursor != null)
        if (device == state.value.sessionDeviceId) mutable.update { it.copy(models = loaded.toList(), modelError = null) }
    }

    suspend fun send(text: String, requestId: String, model: String? = null): Boolean {
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
            is RemoteResult.Failure -> { mutable.update { it.copy(actionError = result.message) }; false }
        }
    }

    suspend fun retryExpiredApproval(requestId: String): Boolean {
        if (!state.value.canRetryExpiredApproval) return false
        mutable.update { it.hideExpiredApproval() }
        return send(EXPIRED_APPROVAL_RETRY, requestId)
    }

    suspend fun interrupt(requestId: String): Boolean {
        val api = client ?: return false
        val session = state.value.selectedSession ?: return false
        val device = state.value.sessionDeviceId ?: return false
        return when (val result = api.empty("/api/devices/$device/sessions/${session.id}/interrupt", body = mapOf("requestId" to requestId))) {
            is RemoteResult.Success -> { loadDetail(); true }
            is RemoteResult.Failure -> { mutable.update { it.copy(actionError = result.message) }; false }
        }
    }

    suspend fun approve(approval: Approval, decision: String) {
        val api = client ?: return
        val session = state.value.selectedSession ?: return
        val device = state.value.sessionDeviceId ?: return
        when (val result = api.empty("/api/devices/$device/sessions/${session.id}/approvals/${approval.id}", body = mapOf("decision" to decision))) {
            is RemoteResult.Success -> loadDetail()
            is RemoteResult.Failure -> handle(result)
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
        vault.remove(state.value.endpoint)
        token = null
        client = null
        mutable.update { RemoteState(gateway = it.gateway, controlAddress = it.controlAddress) }
    }
    private fun fail(message: String) { mutable.update { it.copy(error = message, busy = false, loading = false) } }
    private fun handle(failure: RemoteResult.Failure) { if (failure.status == 401) disconnect(); fail(failure.message) }
}
