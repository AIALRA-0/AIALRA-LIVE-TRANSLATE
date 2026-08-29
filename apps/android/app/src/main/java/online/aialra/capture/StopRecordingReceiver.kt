package online.aialra.capture

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import androidx.core.content.ContextCompat

/** StopRecordingReceiver ends the existing foreground service from the persistent notification action. */
class StopRecordingReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        // Ignore unrelated broadcasts so no external action can control microphone capture.
        if (intent.action != ACTION_STOP_RECORDING) return
        // The existing foreground service drains every durable frame before it releases the server lease.
        ContextCompat.startForegroundService(
            context,
            Intent(context, RecordingService::class.java).setAction(RecordingService.ACTION_GRACEFUL_STOP),
        )
    }

    companion object {
        const val ACTION_STOP_RECORDING = "online.aialra.capture.action.STOP_RECORDING"
    }
}
