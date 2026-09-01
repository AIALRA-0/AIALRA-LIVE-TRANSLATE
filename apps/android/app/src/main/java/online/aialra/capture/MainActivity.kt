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
import okhttp3.Call
import okhttp3.Callback
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import org.json.JSONObject
import java.io.IOException
import java.util.UUID
import java.util.concurrent.TimeUnit

/** The activity pairs once with a selected web course and never asks for internal IDs. */
class MainActivity : ComponentActivity() {
    private lateinit var pairingCode: EditText
    private lateinit var pairButton: Button
    private lateinit var consentCheck: CheckBox
    private lateinit var startButton: Button
    private lateinit var stopButton: Button
    private lateinit var statusText: TextView
    private val networkClient = OkHttpClient.Builder().callTimeout(10, TimeUnit.SECONDS).build()
    private var serverUrl = ""
    private var projectId = ""
    private var sessionId = ""
    private var deviceToken = ""

    private val permissionRequest = registerForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions(),
    ) { results ->
        if (results[Manifest.permission.RECORD_AUDIO] == true) launchRecorder()
        else statusText.text = "麦克风权限被拒绝，录音尚未开始"
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        pairingCode = findViewById(R.id.pairingCode)
        pairButton = findViewById(R.id.pairButton)
        consentCheck = findViewById(R.id.consentCheck)
        startButton = findViewById(R.id.startButton)
        stopButton = findViewById(R.id.stopButton)
        statusText = findViewById(R.id.statusText)

        val preferences = getSharedPreferences("capture", MODE_PRIVATE)
        serverUrl = intent?.data?.getQueryParameter("server")
            ?.trimEnd('/')
            ?: preferences.getString("serverUrl", BuildConfig.PAIRING_SERVER_URL).orEmpty().trimEnd('/')
        pairingCode.setText(intent?.data?.getQueryParameter("code").orEmpty())
        projectId = preferences.getString("projectId", "").orEmpty()
        sessionId = preferences.getString("sessionId", "").orEmpty()
        deviceToken = SecureTokenStore.read(this)

        pairButton.setOnClickListener { pairWithCourse() }
        startButton.setOnClickListener { requestCapture() }
        stopButton.setOnClickListener {
            sendBroadcast(Intent(this, StopRecordingReceiver::class.java).setAction(StopRecordingReceiver.ACTION_STOP_RECORDING))
            showRecording(false, "录音已停止，未确认音频仍保存在应用缓存中")
        }

        val captureActive = RecordingService.isCaptureActive(this)
        val paired = projectId.isNotBlank() && sessionId.isNotBlank() && deviceToken.isNotBlank()
        statusText.text = when {
            captureActive -> "正在后台持续收音，可在这里或通知栏停止录音"
            paired -> "手机已连接到课程，可以开始持续收音"
            serverUrl.isBlank() -> "请从课程网页打开配对链接，或安装已配置服务地址的版本"
            else -> "请在课程网页生成配对码"
        }
        showRecording(captureActive, statusText.text.toString())
        startButton.isEnabled = paired
        if (pairingCode.text.isNotBlank() && serverUrl.isNotBlank()) pairWithCourse()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        val newServer = intent?.data?.getQueryParameter("server")?.trimEnd('/').orEmpty()
        val newCode = intent?.data?.getQueryParameter("code").orEmpty().trim()
        if (newServer.isNotBlank()) serverUrl = newServer
        if (newCode.isNotBlank() && ::pairingCode.isInitialized) {
            pairingCode.setText(newCode)
            if (::statusText.isInitialized && serverUrl.isNotBlank()) pairWithCourse()
        }
    }

    private fun pairWithCourse() {
        val code = pairingCode.text.toString().trim()
        if (serverUrl.isBlank() || code.isBlank()) {
            statusText.text = "配对地址或配对码缺失"
            return
        }
        pairButton.isEnabled = false
        statusText.text = "正在连接课程"
        val deviceId = getSharedPreferences("recording-lease", MODE_PRIVATE)
            .getString("deviceId", null)
            ?: "android-${UUID.randomUUID()}".also {
                getSharedPreferences("recording-lease", MODE_PRIVATE).edit().putString("deviceId", it).apply()
            }
        val body = JSONObject().put("code", code).put("device_id", deviceId).toString()
            .toRequestBody("application/json".toMediaType())
        val request = Request.Builder().url("$serverUrl/api/v1/device-pairing/exchange").post(body).build()
        networkClient.newCall(request).enqueue(object : Callback {
            override fun onFailure(call: Call, e: IOException) = showPairingFailure("无法连接课程服务")

            override fun onResponse(call: Call, response: Response) {
                response.use {
                    if (!it.isSuccessful) return showPairingFailure("配对码无效、过期或已经使用")
                    val payload = runCatching { JSONObject(it.body?.string().orEmpty()) }.getOrNull()
                        ?: return showPairingFailure("课程服务返回了无效结果")
                    projectId = payload.optString("project_id")
                    sessionId = payload.optString("session_id")
                    deviceToken = payload.optString("device_token")
                }
                if (projectId.isBlank() || sessionId.isBlank() || deviceToken.isBlank()) return showPairingFailure("课程连接信息不完整")
                getSharedPreferences("capture", MODE_PRIVATE).edit()
                    .putString("serverUrl", serverUrl)
                    .putString("projectId", projectId)
                    .putString("sessionId", sessionId)
                    .apply()
                SecureTokenStore.write(this@MainActivity, deviceToken)
                runOnUiThread {
                    pairingCode.text.clear()
                    pairButton.isEnabled = true
                    startButton.isEnabled = true
                    statusText.text = "手机已连接到当前课程"
                }
            }
        })
    }

    private fun showPairingFailure(message: String) = runOnUiThread {
        pairButton.isEnabled = true
        statusText.text = message
    }

    private fun requestCapture() {
        if (!consentCheck.isChecked) {
            statusText.text = "请先确认已经获得课程录音许可"
            return
        }
        if (projectId.isBlank() || sessionId.isBlank() || deviceToken.isBlank()) {
            statusText.text = "请先把手机连接到当前课程"
            return
        }
        val permissions = mutableListOf(Manifest.permission.RECORD_AUDIO)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) permissions += Manifest.permission.POST_NOTIFICATIONS
        if (ContextCompat.checkSelfPermission(this, Manifest.permission.RECORD_AUDIO) == PackageManager.PERMISSION_GRANTED) launchRecorder()
        else permissionRequest.launch(permissions.toTypedArray())
    }

    private fun launchRecorder() {
        val intent = Intent(this, RecordingService::class.java)
            .putExtra(RecordingService.EXTRA_SERVER_URL, serverUrl)
            .putExtra(RecordingService.EXTRA_PROJECT_ID, projectId)
            .putExtra(RecordingService.EXTRA_SESSION_ID, sessionId)
            .putExtra(RecordingService.EXTRA_DEVICE_TOKEN, deviceToken)
        ContextCompat.startForegroundService(this, intent)
        showRecording(true, "前台服务已启动，正在等待服务确认音频块")
    }

    private fun showRecording(recording: Boolean, status: String) {
        startButton.visibility = if (recording) View.GONE else View.VISIBLE
        stopButton.visibility = if (recording) View.VISIBLE else View.GONE
        pairButton.visibility = if (recording) View.GONE else View.VISIBLE
        pairingCode.visibility = if (recording) View.GONE else View.VISIBLE
        statusText.text = status
    }
}
