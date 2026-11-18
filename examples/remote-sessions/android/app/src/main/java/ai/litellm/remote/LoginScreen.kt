package ai.litellm.remote

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import java.net.URI
import kotlinx.coroutines.launch

@Composable fun LoginScreen(state: RemoteState, model: RemoteViewModel) {
    var advanced by rememberSaveable { mutableStateOf(false) }
    val browser = LocalUriHandler.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    Column(Modifier.fillMaxSize().imePadding().verticalScroll(rememberScrollState()).padding(horizontal = 28.dp)) {
        Spacer(Modifier.height(38.dp))
        TrainMark(46)
        Spacer(Modifier.height(72.dp))
        Text("Your workspace.\nWithin reach.", fontSize = 41.sp, lineHeight = 44.sp, fontWeight = FontWeight.SemiBold, letterSpacing = (-1.5).sp)
        Spacer(Modifier.height(16.dp))
        Text("Keep building, wherever you are.", color = Secondary, fontSize = 16.sp)
        Spacer(Modifier.height(48.dp))
        if (state.workspace == null) {
            Text("LITELLM GATEWAY", fontSize = 10.sp, fontWeight = FontWeight.SemiBold, color = Secondary, letterSpacing = 1.5.sp)
            Spacer(Modifier.height(12.dp))
            OutlinedTextField(value = state.gateway, onValueChange = model::gateway, placeholder = { Text("gateway.your-company.com", fontSize = 15.sp) },
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri), singleLine = true, modifier = Modifier.fillMaxWidth(), shape = RoundedCornerShape(15.dp))
            TextButton(onClick = { advanced = !advanced }) {
                Text("Advanced", fontSize = 12.sp, color = Secondary)
                Icon(if (advanced) Icons.Default.ExpandLess else Icons.Default.ExpandMore, null, tint = Secondary, modifier = Modifier.size(17.dp))
            }
            if (advanced) OutlinedTextField(value = state.controlAddress, onValueChange = model::controlAddress,
                placeholder = { Text("Remote-service address", fontSize = 13.sp) }, keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                singleLine = true, modifier = Modifier.fillMaxWidth().padding(bottom = 12.dp), shape = RoundedCornerShape(14.dp))
            Button(onClick = model::connect, enabled = state.gateway.isNotBlank() && !state.busy,
                modifier = Modifier.fillMaxWidth().height(56.dp), shape = RoundedCornerShape(15.dp)) {
                if (state.busy) CircularProgressIndicator(Modifier.size(20.dp), color = Color.White, strokeWidth = 2.dp)
                else { Text("Continue", fontWeight = FontWeight.SemiBold); Spacer(Modifier.width(9.dp)); Icon(Icons.AutoMirrored.Filled.ArrowForward, null, modifier = Modifier.size(18.dp)) }
            }
            Spacer(Modifier.height(14.dp))
            Text("Connect to your own gateway. Sign in with your work account.", color = Secondary, fontSize = 12.sp, lineHeight = 18.sp)
        } else {
            Text(runCatching { URI(state.workspace.gateway).host }.getOrNull().orEmpty(), fontSize = 14.sp, color = Secondary)
            Spacer(Modifier.height(22.dp))
            state.login?.let { login ->
                Text("Your sign-in code", fontSize = 14.sp, fontWeight = FontWeight.Medium)
                Spacer(Modifier.height(14.dp))
                Row(Modifier.fillMaxWidth().background(Color.White, RoundedCornerShape(16.dp)).padding(20.dp), verticalAlignment = Alignment.CenterVertically) {
                    Text(login.code, fontSize = 28.sp, fontFamily = FontFamily.Monospace, letterSpacing = 3.sp, fontWeight = FontWeight.SemiBold, modifier = Modifier.weight(1f))
                    IconButton(onClick = {
                        (context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager).setPrimaryClip(ClipData.newPlainText("LiteLLM sign-in code", login.code))
                    }) { Icon(Icons.Default.ContentCopy, "Copy sign-in code", tint = RoyalBlue, modifier = Modifier.size(20.dp)) }
                }
                Spacer(Modifier.height(16.dp))
                Text("Enter this code on the sign-in page, then return here.", fontSize = 14.sp, lineHeight = 21.sp, color = Secondary)
                Spacer(Modifier.height(20.dp))
                Button(onClick = { browser.openUri(login.verificationUrl) }, modifier = Modifier.fillMaxWidth().height(56.dp), shape = RoundedCornerShape(15.dp)) {
                    Text("Open secure sign-in", fontWeight = FontWeight.SemiBold); Spacer(Modifier.width(8.dp)); Icon(Icons.Default.NorthEast, null, modifier = Modifier.size(17.dp))
                }
                Spacer(Modifier.height(16.dp))
                if (state.teams.isEmpty()) Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
                    CircularProgressIndicator(Modifier.size(14.dp), strokeWidth = 1.5.dp)
                    Text("Waiting for you to sign in", fontSize = 12.sp, color = Secondary)
                } else state.teams.forEach { team -> OutlinedButton(onClick = { scope.launch { model.pollLogin(team.id) } }) { Text(team.name) } }
            } ?: Button(onClick = model::startLogin, enabled = !state.busy, modifier = Modifier.fillMaxWidth().height(56.dp), shape = RoundedCornerShape(15.dp)) {
                if (state.busy) CircularProgressIndicator(Modifier.size(20.dp), color = Color.White, strokeWidth = 2.dp)
                else { Text("Sign in with LiteLLM", fontWeight = FontWeight.SemiBold); Spacer(Modifier.width(8.dp)); Icon(Icons.Default.NorthEast, null, modifier = Modifier.size(17.dp)) }
            }
            Spacer(Modifier.height(12.dp))
            TextButton(onClick = model::disconnect) { Text("Use another workspace", fontSize = 12.sp, color = Secondary) }
        }
        state.error?.let { Spacer(Modifier.height(18.dp)); ErrorNotice(it) }
        Spacer(Modifier.height(32.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Default.Shield, null, tint = Secondary, modifier = Modifier.size(14.dp))
            Text("Your account. Your devices. Your LiteLLM gateway.", fontSize = 10.sp, color = Secondary)
        }
        Spacer(Modifier.height(48.dp))
    }
}
