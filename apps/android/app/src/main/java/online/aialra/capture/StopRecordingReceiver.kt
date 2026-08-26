package online.aialra.capture

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import java.util.concurrent.TimeUnit

/** StopRecordingReceiver ends the existing foreground service from the persistent notification action. */
class StopRecordingReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        // Ignore unrelated broadcasts so no external action can control microphone capture.
        if (intent.action != ACTION_STOP_RECORDING) return
        // Stop microphone capture immediately before the bounded remote control request begins.
        context.stopService(Intent(context, RecordingService::class.java))
        // Keep the receiver alive while an independent client closes the matching local session.
        val pendingResult = goAsync()
        Thread({
            // A failed control request cannot restart capture after the microphone service has stopped.
            runCatching { requestRemoteSessionStop(context) }
            pendingResult.finish()
        }, "aialra-stop-session").start()
    }

    private fun requestRemoteSessionStop(context: Context) {
        // Stored local routing fields identify only the local core and the current session.
        val preferences = context.getSharedPreferences("capture", Context.MODE_PRIVATE)
        val serverUrl = preferences.getString("serverUrl", "").orEmpty().trimEnd('/')
        val sessionId = preferences.getString("sessionId", "").orEmpty()
        if (serverUrl.isBlank() || sessionId.isBlank()) return
        val controlBaseUrl = serverUrl
            .replaceFirst("wss://", "https://")
            .replaceFirst("ws://", "http://")
        val request = Request.Builder()
            .url("$controlBaseUrl/api/v1/sessions/$sessionId/stop")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        // A separate short-lived client survives RecordingService.onDestroy and cannot hold capture open.
        OkHttpClient.Builder()
            .callTimeout(5, TimeUnit.SECONDS)
            .build()
            .newCall(request)
            .execute()
            .use { }
    }

    companion object {
        const val ACTION_STOP_RECORDING = "online.aialra.capture.action.STOP_RECORDING"
    }
}
