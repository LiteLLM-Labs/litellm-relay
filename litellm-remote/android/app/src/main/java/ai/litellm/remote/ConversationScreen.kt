package ai.litellm.remote

import android.widget.TextView
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.ui.viewinterop.AndroidView
import io.noties.markwon.Markwon
import java.util.UUID
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable fun ConversationScreen(state: RemoteState, model: RemoteViewModel) {
    val session = state.detail?.session ?: state.selectedSession ?: return
    val available = state.hasCurrentDetail
    var draft by rememberSaveable(session.id) { mutableStateOf("") }
    var requestId by rememberSaveable(session.id) { mutableStateOf(UUID.randomUUID().toString()) }
    var selectedModel by rememberSaveable(session.id) { mutableStateOf<String?>(null) }
    var choosingModel by remember { mutableStateOf(false) }
    var choosingAllModels by remember { mutableStateOf(false) }
    var modelSearch by rememberSaveable { mutableStateOf("") }
    var sending by remember { mutableStateOf(false) }
    var deciding by remember { mutableStateOf(false) }
    var stopping by remember { mutableStateOf(false) }
    var stopRequestId by rememberSaveable(session.id) { mutableStateOf(UUID.randomUUID().toString()) }
    var followed by remember(session.id) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val scroll = rememberLazyListState()
    BackHandler { model.closeSession() }
    LaunchedEffect(state.detail?.messages?.lastOrNull()?.id) {
        if (!followed && !state.detail?.messages.isNullOrEmpty()) {
            scroll.scrollToItem((scroll.layoutInfo.totalItemsCount - 1).coerceAtLeast(0))
            followed = true
        }
    }
    Column(Modifier.fillMaxSize().imePadding()) {
        Row(Modifier.fillMaxWidth().background(Color.White).padding(end = 12.dp), verticalAlignment = Alignment.CenterVertically) {
            IconButton(onClick = model::closeSession) { Icon(Icons.AutoMirrored.Filled.ArrowBack, "Back to sessions", tint = RoyalBlue) }
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(3.dp)) {
                Text(session.title, fontSize = 14.sp, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(session.project, fontSize = 11.sp, color = Secondary, maxLines = 1)
            }
            if (session.canStop) IconButton(onClick = { scope.launch {
                stopping = true
                try { if (model.interrupt(stopRequestId)) stopRequestId = UUID.randomUUID().toString() }
                finally { stopping = false }
            } }, enabled = available && !stopping) { Icon(Icons.Default.Stop, "Stop current turn", tint = RoyalBlue) }
            IconButton(onClick = { scope.launch { scroll.animateScrollToItem((scroll.layoutInfo.totalItemsCount - 1).coerceAtLeast(0)) } }) {
                Icon(Icons.Default.ArrowDownward, "Latest message", tint = RoyalBlue, modifier = Modifier.size(19.dp))
            }
        }
        (state.actionError ?: state.error ?: state.detail?.error?.takeUnless { session.status == "handoff_required" } ?: state.modelError)?.let { Box(Modifier.padding(horizontal = 16.dp, vertical = 8.dp)) { ErrorNotice(it) } }
        LazyColumn(state = scroll, modifier = Modifier.weight(1f).fillMaxWidth(), contentPadding = PaddingValues(22.dp), verticalArrangement = Arrangement.spacedBy(22.dp)) {
            if (session.status == "handoff_required") item {
                Surface(color = Color.White, shape = RoundedCornerShape(18.dp)) {
                    Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                        Text("Set up mobile control", fontWeight = FontWeight.SemiBold)
                        SelectionContainer { Text(state.detail?.error ?: "Finish the current turn and hand this session over from Relay on your Mac.",
                            fontSize = 13.sp, fontFamily = FontFamily.Monospace, lineHeight = 21.sp) }
                    }
                }
            }
            if (state.detail?.messages?.isEmpty() == true && session.status != "handoff_required") item { EmptyState("What are we building?", "Send a message to start working in ${session.project}.") }
            if (state.detail?.olderMessages == true) item { Text("Latest messages", color = Secondary, fontSize = 11.sp) }
            items(state.detail?.messages.orEmpty(), key = { it.id }) { message -> ChatMessage(message, session.agentName) }
            items(state.detail?.approvals.orEmpty(), key = { it.id }) { approval ->
                Surface(color = Color.White, shape = RoundedCornerShape(17.dp), border = androidx.compose.foundation.BorderStroke(1.dp, Warning.copy(alpha = 0.3f))) {
                    Column(Modifier.fillMaxWidth().padding(18.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
                        Text("APPROVAL NEEDED", color = Warning, fontSize = 10.sp, fontWeight = FontWeight.Bold, letterSpacing = 1.sp)
                        Text(approval.title, fontWeight = FontWeight.SemiBold, fontSize = 14.sp)
                        SelectionContainer(Modifier.fillMaxWidth().heightIn(max = 240.dp).background(Canvas, RoundedCornerShape(10.dp)).verticalScroll(rememberScrollState()).padding(12.dp)) {
                            Text(approval.text, fontSize = 12.sp, fontFamily = FontFamily.Monospace, lineHeight = 19.sp)
                        }
                        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                            OutlinedButton(onClick = { scope.launch { deciding = true; try { model.approve(approval, "decline") } finally { deciding = false } } }, enabled = available && !deciding) { Text("Decline") }
                            if (approval.canAccept) Button(onClick = { scope.launch { deciding = true; try { model.approve(approval, "accept") } finally { deciding = false } } }, enabled = available && !deciding) { Text("Allow once") }
                        }
                    }
                }
            }
            if (available && session.status == "working") item {
                Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(12.dp), strokeWidth = 1.5.dp)
                    Text("${session.agentName} is working", fontSize = 12.sp, color = Secondary)
                }
            }
        }
        Column(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 10.dp), horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(10.dp)) {
            Surface(color = Color.White, shape = RoundedCornerShape(19.dp), border = androidx.compose.foundation.BorderStroke(1.dp, Border)) {
                Column {
                    Row(Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 7.dp), verticalAlignment = Alignment.CenterVertically) {
                        Box(Modifier.weight(1f)) {
                            Row(Modifier.clickable(enabled = available && !sending && session.canSend) { choosingModel = true }.padding(vertical = 6.dp),
                                verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                                Icon(Icons.Default.AutoAwesome, null, modifier = Modifier.size(12.dp))
                                Text(state.models.firstOrNull { it.id == (selectedModel ?: session.model) }?.label ?: selectedModel ?: session.model ?: "Session model",
                                    fontSize = 11.sp, fontWeight = FontWeight.Medium, maxLines = 1, overflow = TextOverflow.Ellipsis, modifier = Modifier.widthIn(max = 230.dp))
                                Icon(Icons.Default.ExpandMore, "Choose model", modifier = Modifier.size(13.dp))
                            }
                            DropdownMenu(expanded = choosingModel, onDismissRequest = { choosingModel = false }) {
                                DropdownMenuItem(text = { Text("Use session default") }, onClick = { selectedModel = null; requestId = UUID.randomUUID().toString(); choosingModel = false })
                                state.models.take(12).forEach { model -> DropdownMenuItem(text = { Text(model.label) }, onClick = {
                                    selectedModel = model.id; requestId = UUID.randomUUID().toString(); choosingModel = false
                                }, trailingIcon = { if ((selectedModel ?: session.model) == model.id) Icon(Icons.Default.Check, null, modifier = Modifier.size(16.dp)) }) }
                                if (state.models.size > 12) DropdownMenuItem(text = { Text("All models…") }, onClick = { choosingModel = false; choosingAllModels = true })
                                if (state.modelError != null) DropdownMenuItem(text = { Text("Retry model list") }, onClick = { choosingModel = false; scope.launch { model.loadModels() } })
                            }
                        }
                        Text(if (session.status == "working") "Working" else "Gateway", color = Secondary, fontSize = 10.sp)
                    }
                    HorizontalDivider(Modifier.padding(horizontal = 12.dp), color = Border.copy(alpha = 0.6f))
                Row(Modifier.fillMaxWidth().padding(6.dp), verticalAlignment = Alignment.Bottom) {
                    TextField(value = draft, onValueChange = { draft = it; requestId = UUID.randomUUID().toString() }, placeholder = { Text("Message ${session.agentName}…", fontSize = 15.sp) },
                        modifier = Modifier.weight(1f), maxLines = 6, enabled = available && !sending && session.canSend,
                        colors = TextFieldDefaults.colors(focusedContainerColor = Color.Transparent, unfocusedContainerColor = Color.Transparent,
                            disabledContainerColor = Color.Transparent, focusedIndicatorColor = Color.Transparent, unfocusedIndicatorColor = Color.Transparent))
                    FilledIconButton(onClick = { scope.launch {
                        sending = true
                        try {
                        if (model.send(draft, requestId, selectedModel)) {
                            draft = ""; requestId = UUID.randomUUID().toString()
                            scroll.animateScrollToItem((scroll.layoutInfo.totalItemsCount - 1).coerceAtLeast(0))
                        }
                        } finally { sending = false }
                    } }, enabled = available && draft.isNotBlank() && draft.length <= 32000 && !sending && session.canSend, shape = RoundedCornerShape(12.dp), modifier = Modifier.padding(bottom = 3.dp).size(39.dp)) {
                        if (sending) CircularProgressIndicator(Modifier.size(16.dp), color = Color.White, strokeWidth = 2.dp)
                        else Icon(Icons.Default.ArrowUpward, "Send message", modifier = Modifier.size(19.dp))
                    }
                }
                }
            }
            Text("Via your LiteLLM gateway", fontSize = 10.sp, color = Secondary)
        }
    }
    if (choosingAllModels) ModalBottomSheet(onDismissRequest = { choosingAllModels = false }, containerColor = Canvas) {
        Column(Modifier.fillMaxWidth().padding(horizontal = 22.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
            Text("Choose model", fontSize = 24.sp, fontWeight = FontWeight.SemiBold)
            OutlinedTextField(value = modelSearch, onValueChange = { modelSearch = it }, placeholder = { Text("Search gateway models") },
                singleLine = true, modifier = Modifier.fillMaxWidth(), shape = RoundedCornerShape(14.dp))
            LazyColumn(Modifier.fillMaxWidth().heightIn(max = 440.dp)) {
                items(state.models.filter { modelSearch.isBlank() || "${it.label} ${it.id}".contains(modelSearch, true) }, key = { it.id }) { option ->
                    ListItem(headlineContent = { Text(option.label, fontSize = 14.sp) }, modifier = Modifier.clickable(enabled = available && !sending && session.canSend) {
                        selectedModel = option.id; requestId = UUID.randomUUID().toString(); choosingAllModels = false
                    }, trailingContent = { if ((selectedModel ?: session.model) == option.id) Icon(Icons.Default.Check, null, tint = RoyalBlue) },
                        colors = ListItemDefaults.colors(containerColor = Canvas))
                }
            }
            Spacer(Modifier.height(16.dp))
        }
    }
}

@Composable private fun ChatMessage(message: Message, agentName: String) {
    var expanded by remember(message.id) { mutableStateOf(false) }
    if (message.role == "activity") {
        Surface(onClick = { expanded = !expanded }, color = Border.copy(alpha = 0.3f), shape = RoundedCornerShape(12.dp)) {
            Column(Modifier.fillMaxWidth().padding(13.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                    Icon(Icons.Default.Terminal, null, tint = Secondary, modifier = Modifier.size(14.dp))
                    Text(message.text.lineSequence().first(), color = Secondary, fontFamily = FontFamily.Monospace, fontSize = 11.sp, maxLines = 1, overflow = TextOverflow.Ellipsis,
                        modifier = Modifier.weight(1f))
                    Icon(if (expanded) Icons.Default.ExpandLess else Icons.Default.ExpandMore, if (expanded) "Collapse activity" else "Expand activity", modifier = Modifier.size(14.dp), tint = Secondary)
                }
                if (expanded) SelectionContainer { Text(message.text, color = Secondary, fontFamily = FontFamily.Monospace, fontSize = 11.sp, lineHeight = 18.sp) }
            }
        }
    } else if (message.role == "user") {
        BoxWithConstraints(Modifier.fillMaxWidth()) {
            Column(Modifier.align(Alignment.CenterEnd).widthIn(max = maxWidth - 48.dp)
                .background(RoyalBlue.copy(alpha = 0.09f), RoundedCornerShape(20.dp))
                .border(1.dp, RoyalBlue.copy(alpha = 0.1f), RoundedCornerShape(20.dp)).padding(horizontal = 17.dp, vertical = 13.dp),
                verticalArrangement = Arrangement.spacedBy(10.dp)) {
                SelectionContainer {
                    Text(if (expanded) message.text else message.text.take(8000), color = Ink, fontSize = 15.sp, lineHeight = 23.sp)
                }
                if (message.text.length > 8000 && !expanded) TextButton(onClick = { expanded = true }) { Text("Show full message", fontSize = 12.sp) }
            }
        }
    } else {
        Column(Modifier.fillMaxWidth().background(Color.White, RoundedCornerShape(20.dp)).border(1.dp, Border.copy(alpha = 0.7f), RoundedCornerShape(20.dp)).padding(17.dp), verticalArrangement = Arrangement.spacedBy(13.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(9.dp), verticalAlignment = Alignment.CenterVertically) {
                Box(Modifier.size(30.dp).background(Color.White, RoundedCornerShape(10.dp)), contentAlignment = Alignment.Center) { TrainMark(22) }
                Text(agentName, fontSize = 12.sp, fontWeight = FontWeight.SemiBold)
            }
            val context = LocalContext.current
            val markdown = remember(context) { Markwon.create(context) }
            val text = message.text
            val parsed by produceState<CharSequence>(text, text, markdown) {
                value = text
                value = parseMarkdown(text) { markdown.toMarkdown(it) }
            }
            AndroidView(factory = { TextView(it).apply { textSize = 15f; setTextColor(Ink.toArgb()); setTextIsSelectable(true); setLineSpacing(6f, 1.08f) } },
                update = { if (it.tag !== parsed) { it.text = parsed; it.tag = parsed } }, modifier = Modifier.fillMaxWidth())
        }
    }
}
