package ai.litellm.remote

import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

val RoyalBlue = Color(0xFF0117BE)
val Canvas = Color(0xFFF7F8FB)
val Ink = Color(0xFF111827)
val Secondary = Color(0xFF6B7280)
val Border = Color(0xFFE5E7EB)
val Success = Color(0xFF178044)
val Warning = Color(0xFFA6570F)

@Composable fun RemoteTheme(content: @Composable () -> Unit) {
    MaterialTheme(colorScheme = lightColorScheme(primary = RoyalBlue, background = Canvas, surface = Color.White,
        onPrimary = Color.White, onBackground = Ink, onSurface = Ink, onSurfaceVariant = Secondary, outline = Border), content = content)
}

@Composable fun TrainMark(size: Int = 34) {
    Image(painterResource(R.drawable.litellm_train), contentDescription = "LiteLLM", modifier = Modifier.size(size.dp))
}

@Composable fun ErrorNotice(text: String) {
    Text(text, color = Warning, fontSize = 12.sp, lineHeight = 18.sp,
        modifier = Modifier.fillMaxWidth().background(Warning.copy(alpha = 0.07f), RoundedCornerShape(12.dp)).padding(14.dp))
}

@Composable fun OnlineBadge(online: Boolean) {
    Row(horizontalArrangement = Arrangement.spacedBy(6.dp), verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
        Box(Modifier.size(5.dp).background(if (online) Success else Secondary, RoundedCornerShape(3.dp)))
        Text(if (online) "Online" else "Offline", color = if (online) Success else Secondary, fontSize = 11.sp)
    }
}
