package ai.litellm.remote

import androidx.compose.foundation.*
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.net.URI
import java.util.UUID
import kotlinx.coroutines.launch

@Composable fun RemoteApp(state: RemoteState, model: RemoteViewModel) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val signedIn = state.workspace?.identity != null
    Scaffold(containerColor = Canvas, bottomBar = {
        if (signedIn && state.selectedSession == null) NavigationBar(containerColor = Color.White, tonalElevation = 0.dp) {
            listOf("Sessions" to Icons.Default.ChatBubbleOutline, "Devices" to Icons.Default.LaptopMac, "Account" to Icons.Default.AccountCircle).forEachIndexed { index, item ->
                NavigationBarItem(selected = tab == index, onClick = { tab = index }, icon = { Icon(item.second, null) },
                    label = { Text(item.first, fontSize = 11.sp) }, colors = NavigationBarItemDefaults.colors(indicatorColor = RoyalBlue.copy(alpha = 0.07f), selectedIconColor = RoyalBlue, selectedTextColor = RoyalBlue))
            }
        }
    }) { padding ->
        Box(Modifier.fillMaxSize().padding(padding)) {
            when {
                !signedIn -> LoginScreen(state, model)
                state.selectedSession != null -> ConversationScreen(state, model)
                tab == 0 -> SessionsScreen(state, model)
                tab == 1 -> DevicesScreen(state, model)
                else -> AccountScreen(state, model)
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable private fun SessionsScreen(state: RemoteState, model: RemoteViewModel) {
    var search by rememberSaveable { mutableStateOf("") }
    var chooseDevice by remember { mutableStateOf(false) }
    var newSession by remember { mutableStateOf(false) }
    val sessions = state.sessions.filter { search.isBlank() || "${it.title} ${it.project}".contains(search, true) }.sortedByDescending { it.updatedAt }
    val working = sessions.filter { it.status == "working" || it.needsAttention }
    val recent = sessions.filter { it.status != "working" && !it.needsAttention }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(horizontal = 22.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
        Spacer(Modifier.height(2.dp))
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            TrainMark()
            Spacer(Modifier.weight(1f))
            Text("${state.devices.count { it.online }} connected", color = Secondary, fontSize = 11.sp)
            Icon(Icons.Default.CheckCircleOutline, "Connection status", tint = Success, modifier = Modifier.padding(start = 8.dp).size(18.dp))
        }
        Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                Text("Sessions", fontSize = 35.sp, fontWeight = FontWeight.Bold, letterSpacing = (-1.2).sp)
                Text("Pick up where you left off.", color = Secondary, fontSize = 14.sp)
            }
            FilledIconButton(onClick = { newSession = true }, enabled = state.device?.online == true,
                modifier = Modifier.size(48.dp), shape = RoundedCornerShape(16.dp)) { Icon(Icons.Default.Add, "New session") }
        }
        state.device?.let { device ->
            Box {
                Surface(onClick = { chooseDevice = true }, shape = RoundedCornerShape(14.dp), color = Color.White) {
                    Row(Modifier.fillMaxWidth().padding(15.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(9.dp)) {
                        Icon(Icons.Default.LaptopMac, null, modifier = Modifier.size(17.dp))
                        Text(device.name, fontSize = 12.sp, fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.weight(1f))
                        Icon(Icons.Default.ExpandMore, null, tint = Secondary, modifier = Modifier.size(16.dp))
                        OnlineBadge(device.online)
                    }
                }
                DropdownMenu(expanded = chooseDevice, onDismissRequest = { chooseDevice = false }) {
                    state.devices.forEach { item -> DropdownMenuItem(text = { Text(item.name) }, onClick = { chooseDevice = false; model.selectDevice(item.id) }) }
                }
            }
        }
        if (state.sessions.isNotEmpty()) OutlinedTextField(value = search, onValueChange = { search = it }, modifier = Modifier.fillMaxWidth(),
            placeholder = { Text("Search sessions", fontSize = 14.sp) }, leadingIcon = { Icon(Icons.Default.Search, null, modifier = Modifier.size(19.dp)) },
            shape = RoundedCornerShape(14.dp), singleLine = true)
        state.error?.let { ErrorNotice(it) }
        if (state.loading && state.sessions.isEmpty()) Box(Modifier.fillMaxWidth().padding(50.dp), contentAlignment = Alignment.Center) { CircularProgressIndicator(Modifier.size(24.dp), strokeWidth = 2.dp) }
        else if (sessions.isEmpty()) {
            EmptyState(if (state.devices.isEmpty()) "Bring your Mac along" else if (search.isNotBlank()) "No matches" else "A fresh start",
                if (state.devices.isEmpty()) "Connect the Relay Remote connector to this workspace on your Mac. Your sessions appear here automatically."
                else if (search.isNotBlank()) "Try another project or session name." else "Start a session with the + button. Your work runs through LiteLLM.")
        }
        if (working.isNotEmpty()) SessionSection("IN PROGRESS", working, model::openSession)
        if (recent.isNotEmpty()) SessionSection("RECENT", recent, model::openSession)
        if (state.nextCursor != null) {
            val scope = rememberCoroutineScope()
            TextButton(onClick = { scope.launch { model.refreshSessions(true) } }, modifier = Modifier.fillMaxWidth()) { Text("Load older sessions") }
        }
        Spacer(Modifier.height(8.dp))
    }
    if (newSession) ModalBottomSheet(onDismissRequest = { newSession = false }, containerColor = Canvas) {
        NewSessionSheet(state, model) { newSession = false }
    }
}

@Composable private fun SessionSection(title: String, sessions: List<CodingSession>, open: (CodingSession) -> Unit) {
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("$title  ${sessions.size}", color = Secondary, fontSize = 10.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 1.sp,
            modifier = Modifier.padding(top = 8.dp, bottom = 2.dp))
        sessions.forEach { session ->
            val color = if (session.needsAttention) Warning else if (session.status == "working") RoyalBlue else Secondary
            Surface(onClick = { open(session) }, color = Color.White, shape = RoundedCornerShape(18.dp),
                border = BorderStroke(1.dp, if (session.status == "working") RoyalBlue.copy(alpha = 0.15f) else Border.copy(alpha = 0.5f))) {
                Column(Modifier.fillMaxWidth().padding(19.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Icon(Icons.Default.FolderOpen, null, tint = Secondary, modifier = Modifier.size(13.dp))
                        Text(session.project, fontSize = 11.sp, color = Secondary, maxLines = 1, modifier = Modifier.weight(1f))
                    }
                    Text(session.title, fontSize = 16.sp, fontWeight = FontWeight.SemiBold, letterSpacing = (-0.3).sp, lineHeight = 23.sp, maxLines = 2, overflow = TextOverflow.Ellipsis)
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Box(Modifier.size(5.dp).background(color, RoundedCornerShape(3.dp)))
                        Text(session.activity, color = color, fontSize = 10.sp)
                        Spacer(Modifier.weight(1f))
                        Text(session.agentName, color = Secondary, fontSize = 10.sp)
                        Icon(Icons.Default.ChevronRight, null, tint = Secondary, modifier = Modifier.size(13.dp))
                    }
                }
            }
        }
    }
}

@Composable private fun NewSessionSheet(state: RemoteState, model: RemoteViewModel, close: () -> Unit) {
    var cwd by rememberSaveable { mutableStateOf(state.sessions.firstOrNull()?.cwd.orEmpty()) }
    var agent by rememberSaveable { mutableStateOf("codex") }
    var requestId by rememberSaveable { mutableStateOf(UUID.randomUUID().toString()) }
    val scope = rememberCoroutineScope()
    Column(Modifier.fillMaxWidth().imePadding().verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
        Text("New session", fontSize = 28.sp, fontWeight = FontWeight.Bold, letterSpacing = (-0.8).sp)
        Text("A new conversation. Your project, connected through LiteLLM.", color = Secondary, fontSize = 14.sp, lineHeight = 21.sp)
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            listOf("codex" to "Codex", "claude" to "Claude Code").forEach { (id, title) ->
                FilterChip(selected = agent == id, onClick = { agent = id; requestId = UUID.randomUUID().toString() }, label = { Text(title) }, enabled = !state.busy)
            }
        }
        OutlinedTextField(value = cwd, onValueChange = { cwd = it; requestId = UUID.randomUUID().toString() }, label = { Text("Project folder on your Mac") },
            placeholder = { Text("/Users/you/project") }, modifier = Modifier.fillMaxWidth(), singleLine = true, shape = RoundedCornerShape(14.dp), enabled = !state.busy)
        state.sessions.map { it.cwd }.distinct().take(4).forEach { folder ->
            TextButton(onClick = { cwd = folder; requestId = UUID.randomUUID().toString() }, enabled = !state.busy) {
                Icon(Icons.Default.FolderOpen, null, modifier = Modifier.size(16.dp)); Spacer(Modifier.width(8.dp)); Text(folder, fontSize = 12.sp, maxLines = 1)
            }
        }
        state.error?.let { ErrorNotice(it) }
        Button(onClick = { scope.launch { if (model.create(cwd, agent, requestId)) close() } }, enabled = cwd.startsWith("/") && !state.busy,
            modifier = Modifier.fillMaxWidth().height(54.dp), shape = RoundedCornerShape(15.dp)) {
            if (state.busy) CircularProgressIndicator(Modifier.size(20.dp), color = Color.White, strokeWidth = 2.dp) else Text("Start session", fontWeight = FontWeight.SemiBold)
        }
        Spacer(Modifier.height(12.dp))
    }
}

@Composable fun EmptyState(title: String, description: String) {
    Column(Modifier.fillMaxWidth().padding(horizontal = 20.dp, vertical = 55.dp), horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Icon(Icons.Default.Terminal, null, tint = RoyalBlue, modifier = Modifier.size(76.dp).background(RoyalBlue.copy(alpha = 0.05f), RoundedCornerShape(24.dp)).padding(23.dp))
        Text(title, fontSize = 20.sp, fontWeight = FontWeight.SemiBold, letterSpacing = (-0.4).sp)
        Text(description, color = Secondary, fontSize = 14.sp, lineHeight = 22.sp, textAlign = androidx.compose.ui.text.style.TextAlign.Center)
    }
}

@Composable private fun DevicesScreen(state: RemoteState, model: RemoteViewModel) {
    var setup by remember { mutableStateOf(false) }
    var removing by remember { mutableStateOf<Device?>(null) }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(24.dp)) {
        TrainMark()
        Text("Your devices", fontSize = 34.sp, fontWeight = FontWeight.Bold, letterSpacing = (-1.2).sp)
        Text("Your workspace stays with you. Your files stay where they belong.", color = Secondary, fontSize = 14.sp, lineHeight = 22.sp)
        state.devices.forEach { device ->
            Surface(color = Color.White, shape = RoundedCornerShape(20.dp)) {
                Column(Modifier.fillMaxWidth().padding(20.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(16.dp)) {
                        Icon(Icons.Default.LaptopMac, null, modifier = Modifier.size(55.dp).background(Canvas, RoundedCornerShape(17.dp)).padding(13.dp))
                        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(7.dp)) {
                            Text(device.name, fontWeight = FontWeight.SemiBold, fontSize = 14.sp, maxLines = 1, overflow = TextOverflow.Ellipsis)
                            OnlineBadge(device.online)
                        }
                        IconButton(onClick = { removing = device }) { Icon(Icons.Default.LinkOff, "Disconnect device", tint = Secondary, modifier = Modifier.size(18.dp)) }
                    }
                    HorizontalDivider(color = Border)
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text(device.platform, color = Secondary, fontSize = 11.sp)
                        Text("Encrypted connection", color = Secondary, fontSize = 10.sp)
                    }
                }
            }
        }
        OutlinedButton(onClick = { setup = true }, shape = RoundedCornerShape(15.dp), modifier = Modifier.fillMaxWidth().height(52.dp)) {
            Icon(Icons.Default.Add, null); Spacer(Modifier.width(8.dp)); Text("Connect a device")
        }
        state.error?.let { ErrorNotice(it) }
    }
    if (setup) AlertDialog(onDismissRequest = { setup = false }, title = { Text("Connect your Mac") },
        text = { Text("Start the Relay Remote connector on your Mac and sign in to this workspace. Your Mac appears here automatically. No IP address or port forwarding is needed.") },
        confirmButton = { TextButton(onClick = { setup = false }) { Text("Got it") } })
    removing?.let { device -> AlertDialog(onDismissRequest = { removing = null }, title = { Text("Disconnect ${device.name}?") },
        text = { Text("This revokes remote access. Your coding sessions and files are not deleted.") },
        confirmButton = { TextButton(onClick = { model.removeDevice(device.id); removing = null }) { Text("Disconnect", color = MaterialTheme.colorScheme.error) } },
        dismissButton = { TextButton(onClick = { removing = null }) { Text("Cancel") } }) }
}

@Composable private fun AccountScreen(state: RemoteState, model: RemoteViewModel) {
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(26.dp)) {
        TrainMark()
        Text("Account", fontSize = 34.sp, fontWeight = FontWeight.Bold, letterSpacing = (-1.2).sp)
        Surface(color = Color.White, shape = RoundedCornerShape(20.dp)) {
            Column(Modifier.fillMaxWidth().padding(22.dp), verticalArrangement = Arrangement.spacedBy(18.dp)) {
                Text("Your LiteLLM workspace", fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                Text(runCatching { URI(state.endpoint).host }.getOrNull().orEmpty(), fontSize = 14.sp, color = Secondary)
                HorizontalDivider(color = Border)
                Text("Signed in securely with SSO", color = Success, fontSize = 12.sp)
            }
        }
        Surface(color = Color.White, shape = RoundedCornerShape(20.dp)) {
            Column(Modifier.fillMaxWidth().padding(22.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                Text("CONNECTED GATEWAY", fontSize = 10.sp, letterSpacing = 1.sp, color = Secondary)
                Text(state.workspace?.gateway.orEmpty(), fontWeight = FontWeight.Medium, fontSize = 14.sp)
                Text("Inference runs through your gateway. Your provider credentials never need to be on this phone.", fontSize = 12.sp, color = Secondary, lineHeight = 19.sp)
            }
        }
        state.error?.let { ErrorNotice(it) }
        OutlinedButton(onClick = model::signOut, modifier = Modifier.fillMaxWidth().height(52.dp), shape = RoundedCornerShape(15.dp)) { Text("Sign out", color = MaterialTheme.colorScheme.error) }
        Text("LiteLLM Remote · Preview", fontSize = 11.sp, color = Secondary, modifier = Modifier.align(Alignment.CenterHorizontally))
    }
}
