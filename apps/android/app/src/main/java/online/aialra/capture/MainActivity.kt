package online.aialra.capture

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.TextView
import androidx.activity.ComponentActivity
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat

/** MainActivity collects only the local server address, session ID, and consent confirmation. */
class MainActivity : ComponentActivity() {
    private lateinit var serverUrl: EditText
    private lateinit var sessionId: EditText
    private lateinit var consentCheck: CheckBox
    private lateinit var startButton: Button
    private lateinit var stopButton: Button
    private lateinit var statusText: TextView

    // Permission results start capture only when the microphone permission is present.
    private val permissionRequest = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { results ->
        if (results[Manifest.permission.RECORD_AUDIO] == true) {
            launchRecorder()
        } else {
            statusText.text = "麦克风权限被拒绝，录音尚未开始。"
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // The native screen remains usable when the desktop web application is unavailable.
        setContentView(R.layout.activity_main)
        serverUrl = findViewById(R.id.serverUrl)
        sessionId = findViewById(R.id.sessionId)
        consentCheck = findViewById(R.id.consentCheck)
        startButton = findViewById(R.id.startButton)
        stopButton = findViewById(R.id.stopButton)
        statusText = findViewById(R.id.statusText)

        // Explicit USB launch extras fill only the local address and session ID, never a credential or transcript.
        val requestedServerUrl = intent?.getStringExtra(RecordingService.EXTRA_SERVER_URL)
        val requestedSessionId = intent?.getStringExtra(RecordingService.EXTRA_SESSION_ID)

        // Last-used connection fields reduce setup time when no explicit USB values are supplied.
        val preferences = getSharedPreferences("capture", MODE_PRIVATE)
        serverUrl.setText(requestedServerUrl ?: preferences.getString("serverUrl", "ws://192.0.2.2:8787"))
        sessionId.setText(requestedSessionId ?: preferences.getString("sessionId", ""))

        // Start and stop controls always keep the system foreground notification in sync.
        startButton.setOnClickListener { requestCapture() }
        stopButton.setOnClickListener {
            // The app-local broadcast stops capture first and then closes the matching remote session.
            sendBroadcast(
                Intent(this, StopRecordingReceiver::class.java)
                    .setAction(StopRecordingReceiver.ACTION_STOP_RECORDING),
            )
            showRecording(false, "录音已停止；未确认音频仍保存在应用缓存中。")
        }
        // Returning from the notification must preserve the active service state instead of hiding stop.
        val captureActive = RecordingService.isCaptureActive(this)
        val status = if (captureActive) {
            "正在后台持续收音；可在这里或通知栏停止录音。"
        } else {
            "等待连接。DingTalk A1 可同时作为高质量同步录音。"
        }
        showRecording(captureActive, status)
    }

    private fun requestCapture() {
        // Consent and both routing fields are required before Android asks for system permissions.
        if (!consentCheck.isChecked) {
            statusText.text = "请先确认已经获得课程录音许可。"
            return
        }
        if (serverUrl.text.isBlank() || sessionId.text.isBlank()) {
            statusText.text = "请填写电脑地址和桌面端会话 ID。"
            return
        }
        val permissions = mutableListOf(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            permissions += Manifest.permission.POST_NOTIFICATIONS
        }
        if (ContextCompat.checkSelfPermission(
                this,
                Manifest.permission.RECORD_AUDIO,
            ) == PackageManager.PERMISSION_GRANTED
        ) {
            launchRecorder()
        } else {
            permissionRequest.launch(permissions.toTypedArray())
        }
    }

    private fun launchRecorder() {
        // The server URL and session ID become explicit foreground-service extras for process recovery.
        val normalizedBase = serverUrl.text.toString().trim().trimEnd('/')
        val session = sessionId.text.toString().trim()
        getSharedPreferences("capture", MODE_PRIVATE).edit()
            .putString("serverUrl", normalizedBase)
            .putString("sessionId", session)
            .apply()
        val intent = Intent(this, RecordingService::class.java)
            .putExtra(RecordingService.EXTRA_SERVER_URL, normalizedBase)
            .putExtra(RecordingService.EXTRA_SESSION_ID, session)
        ContextCompat.startForegroundService(this, intent)
        showRecording(true, "前台服务已启动，正在等待本地服务确认音频块。")
    }

    private fun showRecording(recording: Boolean, status: String) {
        // The activity and persistent notification show the same visible recording state.
        startButton.visibility = if (recording) View.GONE else View.VISIBLE
        stopButton.visibility = if (recording) View.VISIBLE else View.GONE
        statusText.text = status
    }
}
