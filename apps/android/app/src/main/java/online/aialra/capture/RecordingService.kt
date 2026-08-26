package online.aialra.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import okhttp3.Callback
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString.Companion.toByteString
import org.json.JSONObject
import java.io.File
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean

/** RecordingService captures mono PCM, persists each frame, and deletes it only after server ACK. */
class RecordingService : Service() {
    private val running = AtomicBoolean(false)
    private val remoteSessionStarted = AtomicBoolean(false) // Remote state prevents the stop request from targeting an unstarted session.
    private val networkClient = OkHttpClient.Builder()
        .callTimeout(5, TimeUnit.SECONDS) // A blocked USB or Wi-Fi control call cannot keep a foreground service alive indefinitely.
        .pingInterval(15, TimeUnit.SECONDS)
        .build()
    private val reconnectExecutor = Executors.newSingleThreadScheduledExecutor()
    private val inFlight = mutableSetOf<Long>()
    private var recorder: AudioRecord? = null
    private var socket: WebSocket? = null
    private var captureThread: Thread? = null
    private lateinit var sessionId: String
    private lateinit var controlBaseUrl: String // HTTP control uses the same trusted local route as the WebSocket audio connection.
    private lateinit var websocketUrl: String
    private lateinit var cacheDirectory: File

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        // Missing extras stop safely before microphone access or network activity begins.
        sessionId = intent?.getStringExtra(EXTRA_SESSION_ID).orEmpty()
        val serverBase = intent?.getStringExtra(EXTRA_SERVER_URL).orEmpty().trimEnd('/')
        if (sessionId.isBlank() || !serverBase.startsWith("ws")) {
            stopSelf()
            return START_NOT_STICKY
        }
        controlBaseUrl = serverBase // Convert the audio route into the matching local HTTP control route.
            .replaceFirst("wss://", "https://")
            .replaceFirst("ws://", "http://")
        websocketUrl = "$serverBase/api/v1/sessions/$sessionId/sources/android/audio"
        cacheDirectory = File(getExternalFilesDir("audio-cache"), safeSessionId(sessionId))
        cacheDirectory.mkdirs()
        setCaptureActive(true)
        startVisibleNotification()
        if (running.compareAndSet(false, true)) {
            startRemoteSessionThenCapture()
        }
        return START_REDELIVER_INTENT
    }

    override fun onDestroy() {
        // Capture stops before network shutdown so the final completed frame remains recoverable on disk.
        running.set(false)
        setCaptureActive(false)
        recorder?.runCatching { stop() }
        recorder?.release()
        recorder = null
        captureThread?.join(1_500)
        socket?.close(1000, "recording stopped")
        if (remoteSessionStarted.get()) requestRemoteSessionStop() // Only a server-confirmed recording session receives a drain request.
        reconnectExecutor.shutdownNow()
        networkClient.dispatcher.executorService.shutdown()
        super.onDestroy()
    }

    private fun startVisibleNotification() {
        // Android requires a persistent channel and notification for long microphone sessions.
        val manager = getSystemService(NotificationManager::class.java)
        manager.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "课堂录音",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { description = "AIALRA 正在持续收音并等待本地服务确认" },
        )
        val launchIntent = Intent(this, MainActivity::class.java)
        val pendingIntent = PendingIntent.getActivity(
            this,
            0,
            launchIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        // An internal broadcast stops the existing service without requesting a second foreground service.
        val stopIntent = Intent(this, StopRecordingReceiver::class.java)
            .setAction(StopRecordingReceiver.ACTION_STOP_RECORDING)
        val stopPendingIntent = PendingIntent.getBroadcast(
            this,
            1,
            stopIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_btn_speak_now)
            .setContentTitle("AIALRA 正在收音")
            .setContentText("音频先保存到手机，收到电脑确认后再清理。")
            .setContentIntent(pendingIntent)
            .addAction(0, "停止录音", stopPendingIntent)
            .setOngoing(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE)
        } else {
            startForeground(NOTIFICATION_ID, notification)
        }
    }

    private fun startAudioCapture() {
        // The service validates permission again because Android may recreate it without the activity.
        if (ContextCompat.checkSelfPermission(
                this,
                Manifest.permission.RECORD_AUDIO,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            stopSelf()
            return
        }
        val minimum = AudioRecord.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_IN_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val bufferBytes = maxOf(minimum, SAMPLE_RATE * 2)
        recorder = AudioRecord.Builder()
            .setAudioSource(MediaRecorder.AudioSource.VOICE_RECOGNITION)
            .setAudioFormat(
                AudioFormat.Builder()
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .build(),
            )
            .setBufferSizeInBytes(bufferBytes * 2)
            .build()
        recorder?.startRecording()
        captureThread = Thread({ captureLoop(bufferBytes) }, "aialra-audio-capture").apply { start() }
    }

    private fun startRemoteSessionThenCapture() {
        // The core must enter recording state before it accepts a durable Android audio frame.
        val request = Request.Builder()
            .url("$controlBaseUrl/api/v1/sessions/$sessionId/start")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        networkClient.newCall(request).enqueue(object : Callback {
            override fun onFailure(call: okhttp3.Call, e: java.io.IOException) {
                stopSelf()
            }

            override fun onResponse(call: okhttp3.Call, response: Response) {
                response.use {
                    if (!it.isSuccessful) {
                        stopSelf()
                        return
                    }
                }
                remoteSessionStarted.set(true)
                if (!running.get()) return
                connectSocket()
                startAudioCapture()
            }
        })
    }

    private fun requestRemoteSessionStop() {
        // The bounded stop request tells the core to drain accepted model work after phone capture ends.
        val request = Request.Builder()
            .url("$controlBaseUrl/api/v1/sessions/$sessionId/stop")
            .post(ByteArray(0).toRequestBody(null))
            .build()
        runCatching { networkClient.newCall(request).execute().close() }
    }

    private fun captureLoop(bufferBytes: Int) {
        // Each successful read becomes an atomic disk frame before any WebSocket send attempt.
        val shortBuffer = ShortArray(bufferBytes / 2)
        while (running.get()) {
            val read = recorder?.read(shortBuffer, 0, shortBuffer.size) ?: break
            if (read <= 0) continue
            val pcm = ByteBuffer.allocate(read * 2).order(ByteOrder.LITTLE_ENDIAN)
            repeat(read) { index -> pcm.putShort(shortBuffer[index]) }
            val sequence = nextSequence()
            val frame = AudioFrame.encode(sequence, System.currentTimeMillis(), pcm.array())
            val file = frameFile(sequence)
            val temporary = File(cacheDirectory, "${file.name}.tmp")
            temporary.writeBytes(frame)
            if (!temporary.renameTo(file)) {
                temporary.copyTo(file, overwrite = true)
                temporary.delete()
            }
            sendPending()
        }
    }

    @Synchronized
    private fun connectSocket() {
        // A reconnect clears in-flight markers because every stored frame is safe to resend idempotently.
        if (!running.get()) return
        inFlight.clear()
        val request = Request.Builder().url(websocketUrl).build()
        socket = networkClient.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                sendPending()
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                // ACK deletion is the only path that removes a durable audio frame.
                val message = runCatching { JSONObject(text) }.getOrNull() ?: return
                if (message.optString("type") != "audio.ack") return
                val sequence = message.optLong("sequence", -1)
                if (sequence < 0) return
                frameFile(sequence).delete()
                synchronized(this@RecordingService) { inFlight.remove(sequence) }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                scheduleReconnect()
            }
        })
    }

    @Synchronized
    private fun sendPending() {
        // Sorted filenames preserve capture order while allowing duplicates after a broken connection.
        val currentSocket = socket ?: return
        pendingFiles().forEach { file ->
            val sequence = file.nameWithoutExtension.toLongOrNull() ?: return@forEach
            if (sequence in inFlight) return@forEach
            if (currentSocket.send(file.readBytes().toByteString())) {
                inFlight += sequence
            }
        }
    }

    private fun scheduleReconnect() {
        // A short bounded delay preserves battery while recovering common classroom Wi-Fi transitions.
        if (!running.get()) return
        reconnectExecutor.schedule({ connectSocket() }, 2, TimeUnit.SECONDS)
    }

    private fun pendingFiles(): List<File> =
        cacheDirectory.listFiles { file -> file.extension == "frame" }
            ?.sortedBy { it.nameWithoutExtension.toLongOrNull() ?: Long.MAX_VALUE }
            .orEmpty()

    private fun frameFile(sequence: Long): File =
        File(cacheDirectory, sequence.toString().padStart(20, '0') + ".frame")

    private fun nextSequence(): Long {
        // A committed preference advances before capture, preventing sequence reuse after a process crash.
        val preferences = getSharedPreferences("capture-sequence", MODE_PRIVATE)
        val key = "sequence-$sessionId"
        val next = preferences.getLong(key, 0L) + 1L
        preferences.edit().putLong(key, next).commit()
        return next
    }

    private fun safeSessionId(value: String): String =
        value.filter { character -> character.isLetterOrDigit() || character in "_-" }.take(128)

    private fun setCaptureActive(active: Boolean) {
        // A small local flag restores the correct controls when Android recreates the activity.
        getSharedPreferences(CAPTURE_STATE_PREFERENCES, MODE_PRIVATE)
            .edit()
            .putBoolean(CAPTURE_ACTIVE_KEY, active)
            .apply()
    }

    companion object {
        const val EXTRA_SERVER_URL = "serverUrl"
        const val EXTRA_SESSION_ID = "sessionId"
        private const val SAMPLE_RATE = 16_000
        private const val CHANNEL_ID = "aialra-recording"
        private const val NOTIFICATION_ID = 4201
        private const val CAPTURE_STATE_PREFERENCES = "capture-state"
        private const val CAPTURE_ACTIVE_KEY = "active"

        fun isCaptureActive(context: android.content.Context): Boolean =
            // The flag represents a foreground service that has not completed its local shutdown path.
            context.getSharedPreferences(CAPTURE_STATE_PREFERENCES, android.content.Context.MODE_PRIVATE)
                .getBoolean(CAPTURE_ACTIVE_KEY, false)
    }
}
