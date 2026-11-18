package ai.litellm.remote

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val model = ViewModelProvider(this)[RemoteViewModel::class.java]
        setContent {
            val state by model.state.collectAsStateWithLifecycle()
            val owner = LocalLifecycleOwner.current
            LaunchedEffect(owner) {
                owner.lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
                    while (currentCoroutineContext().isActive) {
                        if (model.state.value.login != null && model.state.value.teams.isEmpty()) model.pollLogin()
                        else if (model.state.value.workspace?.identity != null) model.refresh()
                        delay(3000)
                    }
                }
            }
            RemoteTheme { RemoteApp(state, model) }
        }
    }
}
