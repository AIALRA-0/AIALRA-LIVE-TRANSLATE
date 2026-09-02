const TARGET_SAMPLE_RATE = 16_000;
const DATABASE_NAME = "aialra-audio-outbox";
const STORE_NAME = "frames";
const METADATA_STORE_NAME = "metadata";
const MAX_IN_FLIGHT_FRAMES = 8;

export type CaptureMode = "microphone" | "screen";
export type CapturePhase =
  | "idle"
  | "requesting-permission"
  | "acquiring-lease"
  | "connecting"
  | "recording"
  | "blocked"
  | "recoverable"
  | "stopping"
  | "processing"
  | "error";

interface PersistedFrame {
  key: string;
  sessionId: string;
  sourceId: string;
  sequence: number;
  frame: ArrayBuffer;
}

// Big-endian 64-bit fields match the Rust and Android transport contract.
function writeU64(view: DataView, offset: number, value: number): void {
  view.setBigUint64(offset, BigInt(Math.max(0, Math.floor(value))), false);
}

export function resample(input: Float32Array, sourceRate: number): Float32Array {
  if (sourceRate === TARGET_SAMPLE_RATE) return input;
  const ratio = sourceRate / TARGET_SAMPLE_RATE;
  const output = new Float32Array(Math.max(1, Math.floor(input.length / ratio)));
  for (let index = 0; index < output.length; index += 1) {
    const sourceIndex = index * ratio;
    const left = Math.floor(sourceIndex);
    const right = Math.min(left + 1, input.length - 1);
    const fraction = sourceIndex - left;
    output[index] = input[left] * (1 - fraction) + input[right] * fraction;
  }
  return output;
}

export function encodeFrame(sequence: number, capturedAtMs: number, samples: Float32Array): ArrayBuffer {
  const frame = new ArrayBuffer(16 + samples.length * 2);
  const view = new DataView(frame);
  writeU64(view, 0, sequence);
  writeU64(view, 8, capturedAtMs);
  samples.forEach((sample, index) => {
    const clipped = Math.max(-1, Math.min(1, sample));
    const pcm = clipped < 0 ? clipped * 32768 : clipped * 32767;
    view.setInt16(16 + index * 2, Math.round(pcm), true);
  });
  return frame;
}

export function recoverNextSequence(persisted: number | null, pendingSequences: number[]): number {
  const durable = persisted !== null && Number.isSafeInteger(persisted) && persisted >= 1 ? persisted : 1;
  return Math.max(durable, ...pendingSequences.map((sequence) => sequence + 1), 1);
}

export function nextFramesToSend(
  pendingSequences: number[],
  inFlightSequences: number[],
  limit = MAX_IN_FLIGHT_FRAMES,
): number[] {
  const inFlight = new Set(inFlightSequences);
  const available = Math.max(0, limit - inFlight.size);
  return pendingSequences
    .filter((sequence) => !inFlight.has(sequence))
    .sort((left, right) => left - right)
    .slice(0, available);
}

export interface DurableAudioAck {
  type: "audio.ack";
  sequence: number;
  commit_id: string;
}

// A frame is safe to delete locally only after Core has returned its durable
// SQLite/object-store commit identifier.  Older or malformed responses remain
// pending so a reconnect can retry them instead of turning an ACK into loss.
export function isDurableAudioAck(message: unknown): message is DurableAudioAck {
  if (!message || typeof message !== "object") return false;
  const candidate = message as Partial<DurableAudioAck>;
  return candidate.type === "audio.ack"
    && typeof candidate.sequence === "number"
    && Number.isSafeInteger(candidate.sequence)
    && candidate.sequence >= 1
    && typeof candidate.commit_id === "string"
    && candidate.commit_id.length > 0;
}

function openOutbox(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE_NAME, 2);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(STORE_NAME)) {
        const store = database.createObjectStore(STORE_NAME, { keyPath: "key" });
        store.createIndex("sessionSource", ["sessionId", "sourceId"]);
      }
      if (!database.objectStoreNames.contains(METADATA_STORE_NAME)) {
        database.createObjectStore(METADATA_STORE_NAME, { keyPath: "key" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("无法打开音频恢复缓存"));
  });
}

function transactionRequest<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("音频恢复缓存操作失败"));
  });
}

async function listFrames(sessionId: string, sourceId: string): Promise<PersistedFrame[]> {
  const database = await openOutbox();
  try {
    const transaction = database.transaction(STORE_NAME, "readonly");
    const index = transaction.objectStore(STORE_NAME).index("sessionSource");
    const rows = await transactionRequest(index.getAll(IDBKeyRange.only([sessionId, sourceId])));
    return (rows as PersistedFrame[]).sort((left, right) => left.sequence - right.sequence);
  } finally {
    database.close();
  }
}

function transactionComplete(transaction: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    transaction.oncomplete = () => resolve();
    transaction.onabort = () => reject(transaction.error ?? new Error("音频恢复缓存事务已中止"));
    transaction.onerror = () => reject(transaction.error ?? new Error("音频恢复缓存事务失败"));
  });
}

async function loadNextSequence(key: string): Promise<number | null> {
  const database = await openOutbox();
  try {
    const transaction = database.transaction(METADATA_STORE_NAME, "readonly");
    const value = await transactionRequest(transaction.objectStore(METADATA_STORE_NAME).get(key)) as { nextSequence?: number } | undefined;
    return typeof value?.nextSequence === "number" ? value.nextSequence : null;
  } finally {
    database.close();
  }
}

async function saveFrameAndSequence(record: PersistedFrame, sequenceKey: string, nextSequence: number): Promise<void> {
  const database = await openOutbox();
  try {
    const transaction = database.transaction([STORE_NAME, METADATA_STORE_NAME], "readwrite");
    transaction.objectStore(STORE_NAME).put(record);
    transaction.objectStore(METADATA_STORE_NAME).put({ key: sequenceKey, nextSequence });
    await transactionComplete(transaction);
  } finally {
    database.close();
  }
}

async function deleteFrame(key: string): Promise<void> {
  const database = await openOutbox();
  try {
    const transaction = database.transaction(STORE_NAME, "readwrite");
    await transactionRequest(transaction.objectStore(STORE_NAME).delete(key));
  } finally {
    database.close();
  }
}

function workletModuleUrl(): string {
  const source = `
    class AialraPcmProcessor extends AudioWorkletProcessor {
      process(inputs) {
        const channel = inputs[0] && inputs[0][0];
        if (channel && channel.length) this.port.postMessage(channel.slice(0));
        return true;
      }
    }
    registerProcessor("aialra-pcm", AialraPcmProcessor);
  `;
  return URL.createObjectURL(new Blob([source], { type: "text/javascript" }));
}

function requestMediaWithTimeout(request: Promise<MediaStream>, timeoutMs = 15_000): Promise<MediaStream> {
  return new Promise((resolve, reject) => {
    let settled = false;
    const timer = window.setTimeout(() => {
      settled = true;
      reject(new Error("麦克风权限请求超时，请点击地址栏的权限图标允许收音后重试"));
    }, timeoutMs);
    request.then((stream) => {
      if (settled) stream.getTracks().forEach((track) => track.stop());
      else {
        settled = true;
        window.clearTimeout(timer);
        resolve(stream);
      }
    }).catch((error) => {
      if (settled) return;
      settled = true;
      window.clearTimeout(timer);
      reject(error);
    });
  });
}

// Device labels are intentionally read only after the browser has granted
// microphone permission.  macOS and Chromium otherwise return blank or
// generic labels, which makes it look as if device switching is unavailable.
export async function listAudioInputs(requestPermission = false): Promise<MediaDeviceInfo[]> {
  const mediaDevices = navigator.mediaDevices;
  if (!mediaDevices?.enumerateDevices) throw new Error("当前浏览器无法读取麦克风设备列表");
  let permissionStream: MediaStream | null = null;
  try {
    if (requestPermission) {
      if (!mediaDevices.getUserMedia) throw new Error("当前浏览器不支持麦克风权限请求");
      permissionStream = await requestMediaWithTimeout(mediaDevices.getUserMedia({ audio: true }));
    }
    return (await mediaDevices.enumerateDevices()).filter((device) => device.kind === "audioinput");
  } catch (error) {
    throw new Error(mediaInputError(error));
  } finally {
    permissionStream?.getTracks().forEach((track) => track.stop());
  }
}

export function mediaInputError(error: unknown): string {
  if (error instanceof Error && error.message.startsWith("麦克风权限请求超时")) return error.message;
  const name = typeof error === "object" && error !== null && "name" in error ? String((error as { name?: unknown }).name) : "";
  if (name === "NotAllowedError" || name === "SecurityError") return "麦克风权限被拒绝，请允许当前网站使用麦克风后重试";
  if (name === "NotFoundError" || name === "DevicesNotFoundError") return "没有找到可用麦克风，请连接麦克风或选择其他输入设备";
  if (name === "NotReadableError" || name === "TrackStartError") return "麦克风正在被其他应用占用，请关闭占用它的应用后重试";
  if (name === "OverconstrainedError") return "所选输入设备当前不可用，请重新选择麦克风后重试";
  return "浏览器无法读取音频输入，请检查麦克风权限和设备状态";
}

// AudioWorklet keeps capture off the UI thread while IndexedDB survives refreshes and short outages.
export class BrowserCapture {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private sourceNode: MediaStreamAudioSourceNode | null = null;
  private worklet: AudioWorkletNode | null = null;
  private outputGain: GainNode | null = null;
  private socket: WebSocket | null = null;
  private sequence = 1;
  private pending = new Map<number, ArrayBuffer>();
  private inFlight = new Set<number>();
  private reconnectTimer: number | null = null;
  private renewTimer: number | null = null;
  private socketOpenTimer: number | null = null;
  private stopped = false;
  private stopping = false;
  private prepared = false;
  private activated = false;
  private leaseToken = "";
  private leaseGeneration = 0;
  private sampleChunks: Float32Array[] = [];
  private sampleCount = 0;
  private writeChain = Promise.resolve();
  private sourceId = "";
  private sequenceStorageKey = "";

  constructor(
    private readonly projectId: string,
    private readonly sessionId: string,
    private readonly recorderDeviceId: string,
    private readonly onStatus: (message: string) => void,
    private readonly mode: CaptureMode = "microphone",
    private readonly deviceId?: string,
    private readonly onRevoked?: (message?: string) => void,
  ) {}

  // Prepare the browser input before asking Core for a recording lease.  This
  // keeps denied permissions and unsupported browsers from creating orphaned
  // server-side recording sessions.
  async prepare(): Promise<void> {
    if (this.prepared) return;
    if (!window.isSecureContext && window.location.hostname !== "localhost") {
      throw new Error("浏览器录音需要 HTTPS 安全连接");
    }
    const mediaDevices = navigator.mediaDevices;
    if (!mediaDevices) throw new Error("当前浏览器没有可用的媒体设备，请改用最新版 Chrome 或 Edge");
    if (!("AudioContext" in window) || !("AudioWorkletNode" in window)) {
      throw new Error("当前浏览器不支持低延迟音频采集，请改用最新版 Chrome 或 Edge");
    }
    if (this.mode === "microphone" && !mediaDevices.getUserMedia) {
      throw new Error("当前浏览器不支持麦克风录音，请改用最新版 Chrome 或 Edge");
    }
    if (this.mode === "screen" && !mediaDevices.getDisplayMedia) {
      throw new Error("当前浏览器不支持共享音频，请改用最新版 Chrome 或 Edge");
    }
    try {
      this.stream = this.mode === "screen"
        ? await requestMediaWithTimeout(mediaDevices.getDisplayMedia({ audio: true, video: true }))
        : await requestMediaWithTimeout(mediaDevices.getUserMedia({
            audio: {
              channelCount: 1,
              echoCancellation: true,
              noiseSuppression: true,
              autoGainControl: true,
              ...(this.deviceId ? { deviceId: { exact: this.deviceId } } : {}),
            },
          }));
      if (this.mode === "screen") {
        this.stream.getVideoTracks().forEach((track) => track.stop());
        if (this.stream.getAudioTracks().length === 0) {
          throw new Error("共享内容没有音频，请在浏览器共享框中勾选音频");
        }
      }
      this.context = new AudioContext({ latencyHint: "interactive" });
      const moduleUrl = workletModuleUrl();
      try {
        await this.context.audioWorklet.addModule(moduleUrl);
      } finally {
        URL.revokeObjectURL(moduleUrl);
      }
      this.sourceNode = this.context.createMediaStreamSource(this.stream);
      this.worklet = new AudioWorkletNode(this.context, "aialra-pcm");
      this.worklet.port.onmessage = (event: MessageEvent<Float32Array>) => {
        this.acceptSamples(event.data);
      };
      this.sourceNode.connect(this.worklet);
      // Keep the worklet alive without playing the microphone back through the
      // speakers, which would create an echo loop during a lecture.
      this.outputGain = this.context.createGain();
      this.outputGain.gain.value = 0;
      this.worklet.connect(this.outputGain);
      this.outputGain.connect(this.context.destination);
      await this.context.resume().catch(() => undefined);
      this.prepared = true;
    } catch (error) {
      this.disposeInput();
      throw new Error(this.mediaError(error));
    }
  }

  // Activate an already prepared input with a server-issued lease.  A retry
  // after a transient WebSocket failure can reuse the same lease safely.
  async activate(leaseToken: string, leaseGeneration: number): Promise<void> {
    if (!leaseToken) throw new Error("录音租约无效，请重新开始录音");
    await this.prepare();
    if (this.activated) return;
    this.leaseToken = leaseToken;
    this.leaseGeneration = leaseGeneration;
    this.sourceId = `browser-${this.mode === "screen" ? "screen" : "mic"}-g${leaseGeneration}`;
    this.sequenceStorageKey = `aialra-audio-next-sequence:${this.sessionId}:${this.sourceId}`;
    const recovered = await listFrames(this.sessionId, this.sourceId);
    recovered.forEach((item) => this.pending.set(item.sequence, item.frame));
    const storedSequence = await loadNextSequence(this.sequenceStorageKey);
    this.sequence = recoverNextSequence(
      storedSequence,
      recovered.map((item) => item.sequence),
    );
    this.stopped = false;
    this.stopping = false;
    this.activated = true;
    this.onStatus("正在连接服务器，音频会先保存到本机恢复缓存");
    this.connect();
    this.renewTimer = window.setInterval(() => void this.renewLease(), 10_000);
  }

  // Compatibility wrapper for callers that do not need to display the
  // preparation and lease phases separately.
  async start(leaseToken: string, leaseGeneration: number): Promise<void> {
    await this.prepare();
    await this.activate(leaseToken, leaseGeneration);
  }

  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopping = true;
    this.sourceNode?.disconnect();
    this.worklet?.disconnect();
    this.outputGain?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    await this.context?.close();
    if (this.sampleCount > 0) {
      const tail = this.takeSamples(this.sampleCount);
      this.writeChain = this.writeChain.then(() => this.queueFrame(tail));
    }
    await this.writeChain;
    const deadline = Date.now() + 30_000;
    while (this.pending.size > 0 && Date.now() < deadline) {
      if (this.socket?.readyState !== WebSocket.OPEN) this.connect();
      this.sendPending();
      await new Promise((resolve) => window.setTimeout(resolve, 100));
    }
    if (this.pending.size > 0) {
      this.stopping = false;
      throw new Error(`仍有 ${this.pending.size} 个音频块等待服务器确认，请保持页面在线后再次停止`);
    }
    this.stopped = true;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    if (this.renewTimer !== null) window.clearInterval(this.renewTimer);
    if (this.socketOpenTimer !== null) window.clearTimeout(this.socketOpenTimer);
    this.socket?.close();
    this.activated = false;
    this.onStatus("录音已停止，已采集音频均得到服务器确认");
  }

  // Dispose a prepared input without presenting a lease-takeover message.
  dispose(): void {
    this.revoke(undefined, false);
  }

  revoke(message?: string, notify = true): void {
    const wasActivated = this.activated;
    this.stopped = true;
    this.stopping = false;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    if (this.renewTimer !== null) window.clearInterval(this.renewTimer);
    if (this.socketOpenTimer !== null) window.clearTimeout(this.socketOpenTimer);
    this.disposeInput();
    this.socket?.close();
    this.activated = false;
    if (notify && wasActivated) {
      const notice = message ?? `录音权限已由另一台设备接管，${this.pending.size} 个未确认音频块保留在本机`;
      this.onStatus(notice);
      this.onRevoked?.(notice);
    }
  }

  private disposeInput(): void {
    this.sourceNode?.disconnect();
    this.worklet?.disconnect();
    this.outputGain?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    const context = this.context;
    if (context) void context.close().catch(() => undefined);
    this.sourceNode = null;
    this.worklet = null;
    this.outputGain = null;
    this.stream = null;
    this.context = null;
    this.prepared = false;
  }

  private mediaError(error: unknown): string {
    return mediaInputError(error).replace("读取音频输入", "打开音频输入");
  }

  private acceptSamples(samples: Float32Array): void {
    if (this.stopped || this.stopping) return;
    const resampled = resample(samples, this.context?.sampleRate ?? 48_000);
    this.sampleChunks.push(resampled);
    this.sampleCount += resampled.length;
    while (this.sampleCount >= TARGET_SAMPLE_RATE) {
      const chunk = this.takeSamples(TARGET_SAMPLE_RATE);
      this.writeChain = this.writeChain.then(() => this.queueFrame(chunk));
    }
  }

  private takeSamples(count: number): Float32Array {
    const output = new Float32Array(count);
    let written = 0;
    while (written < count) {
      const current = this.sampleChunks[0];
      const consumed = Math.min(current.length, count - written);
      output.set(current.subarray(0, consumed), written);
      written += consumed;
      if (consumed === current.length) this.sampleChunks.shift();
      else this.sampleChunks[0] = current.subarray(consumed);
    }
    this.sampleCount -= count;
    return output;
  }

  private async queueFrame(samples: Float32Array): Promise<void> {
    const sequence = this.sequence;
    const nextSequence = sequence + 1;
    const frame = encodeFrame(sequence, Date.now(), samples);
    const key = `${this.sessionId}:${this.sourceId}:${sequence}`;
    await saveFrameAndSequence(
      { key, sessionId: this.sessionId, sourceId: this.sourceId, sequence, frame },
      this.sequenceStorageKey,
      nextSequence,
    );
    this.sequence = nextSequence;
    this.pending.set(sequence, frame);
    this.sendPending();
  }

  private connect(): void {
    if (this.stopped || !this.leaseToken) return;
    if (this.reconnectTimer !== null) {
      window.clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    this.socket = new WebSocket(
      `${scheme}://${window.location.host}/api/v1/sessions/${this.sessionId}/sources/${this.sourceId}/audio`,
      ["aialra.audio.v1", `lease.${this.leaseToken}`],
    );
    this.socket.binaryType = "arraybuffer";
    const socket = this.socket;
    this.socketOpenTimer = window.setTimeout(() => {
      if (socket === this.socket && socket.readyState === WebSocket.CONNECTING) {
        this.onStatus("服务器连接超时，音频已安全缓存，正在重试");
        socket.close();
      }
    }, 10_000);
    this.socket.onopen = () => {
      if (this.socketOpenTimer !== null) window.clearTimeout(this.socketOpenTimer);
      this.socketOpenTimer = null;
      this.inFlight.clear();
      this.onStatus(`${this.mode === "screen" ? "共享音频" : "麦克风"}已连接，等待服务器确认音频块`);
      this.sendPending();
    };
    this.socket.onmessage = (event) => {
      let message: { type: string; sequence?: number; commit_id?: string; message?: string };
      try {
        message = JSON.parse(String(event.data)) as { type: string; sequence?: number; commit_id?: string; message?: string };
      } catch {
        this.onStatus("服务器返回了无法识别的音频确认，音频块继续保留并等待重试");
        this.socket?.close();
        return;
      }
      if (isDurableAudioAck(message)) {
        this.inFlight.delete(message.sequence);
        this.pending.delete(message.sequence);
        void deleteFrame(`${this.sessionId}:${this.sourceId}:${message.sequence}`);
        if (this.pending.size === 0) this.onStatus("收音正常，服务器已确认全部音频块");
        this.sendPending();
      } else if (message.type === "audio.ack") {
        this.onStatus("服务器返回了非持久确认，音频块继续保留并等待重试");
        this.socket?.close();
      } else if (message.type === "audio.error") {
        this.onStatus(message.message || "服务器拒绝了音频块，等待连接恢复");
        this.socket?.close();
      }
    };
    this.socket.onerror = () => {
      if (!this.stopped && !this.stopping) this.onStatus(`服务器连接失败，${this.pending.size} 个音频块已安全缓存，正在重试`);
    };
    this.socket.onclose = () => {
      if (this.socketOpenTimer !== null) window.clearTimeout(this.socketOpenTimer);
      this.socketOpenTimer = null;
      if (this.stopped || (this.stopping && this.pending.size === 0)) return;
      this.inFlight.clear();
      this.onStatus(`连接恢复中，${this.pending.size} 个音频块已安全缓存`);
      this.reconnectTimer = window.setTimeout(() => this.connect(), 1_500);
    };
  }

  private sendPending(): void {
    if (this.socket?.readyState !== WebSocket.OPEN) return;
    const sequences = nextFramesToSend([...this.pending.keys()], [...this.inFlight]);
    for (const sequence of sequences) {
      const frame = this.pending.get(sequence);
      if (!frame) continue;
      try {
        this.socket.send(frame);
        this.inFlight.add(sequence);
      } catch {
        this.socket.close();
        return;
      }
    }
  }

  private async renewLease(): Promise<void> {
    if (this.stopped) return;
    let response: Response;
    try {
      response = await fetch(`/api/v1/projects/${this.projectId}/sessions/${this.sessionId}/recording/renew`, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ device_id: this.recorderDeviceId, lease_token: this.leaseToken }),
      });
    } catch {
      this.onStatus(`网络不可用，${this.pending.size} 个未确认音频块已安全缓存`);
      return;
    }
    if (response.ok) return;
    const body = await response.json().catch(() => ({})) as { code?: string };
    const notice = body.code === "recording_lease_conflict"
      ? "录音权限已由另一台设备接管，本机停止采集，未确认块仍保留"
      : body.code === "recording_lease_expired"
        ? "本机录音租约已到期，本机停止采集，未确认块仍保留；状态确认后可以继续本次课程"
        : "录音权限已失效，本机停止采集，未确认块仍保留；请重新检查录音状态";
    this.revoke(notice);
  }
}

export interface MicrophoneTestProgress {
  phase: "quiet" | "speech";
  elapsedMs: number;
  levelDbfs: number;
}

export interface MicrophoneTestResult {
  passed: boolean;
  noiseFloorDbfs: number;
  speechP95Dbfs: number;
  peakDbfs: number;
  clippingRatio: number;
  sampleRate: number;
  message: string;
}

function amplitudeDbfs(value: number): number {
  return value <= 0 ? -96 : Math.max(-96, 20 * Math.log10(value));
}

function percentile(values: number[], ratio: number): number {
  if (values.length === 0) return -96;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * ratio))];
}

// The preflight test reads the selected microphone locally and never opens a recording lease or network request.
export async function testMicrophone(
  deviceId: string | undefined,
  onProgress: (progress: MicrophoneTestProgress) => void,
): Promise<MicrophoneTestResult> {
  if (!navigator.mediaDevices?.getUserMedia) throw new Error("当前浏览器不支持麦克风测试");
  const stream = await navigator.mediaDevices.getUserMedia({
    audio: {
      channelCount: 1,
      echoCancellation: true,
      noiseSuppression: true,
      autoGainControl: true,
      ...(deviceId ? { deviceId: { exact: deviceId } } : {}),
    },
  });
  const context = new AudioContext({ latencyHint: "interactive" });
  const analyser = context.createAnalyser();
  analyser.fftSize = 2048;
  const source = context.createMediaStreamSource(stream);
  source.connect(analyser);
  const samples = new Float32Array(analyser.fftSize);
  const quietLevels: number[] = [];
  const speechLevels: number[] = [];
  let peak = 0;
  let clipped = 0;
  let total = 0;
  const started = performance.now();
  try {
    while (performance.now() - started < 4_000) {
      analyser.getFloatTimeDomainData(samples);
      let squared = 0;
      for (const sample of samples) {
        const absolute = Math.abs(sample);
        squared += sample * sample;
        peak = Math.max(peak, absolute);
        if (absolute >= 0.891) clipped += 1;
        total += 1;
      }
      const level = amplitudeDbfs(Math.sqrt(squared / samples.length));
      const elapsedMs = performance.now() - started;
      if (elapsedMs < 1_000) quietLevels.push(level);
      else speechLevels.push(level);
      onProgress({ phase: elapsedMs < 1_000 ? "quiet" : "speech", elapsedMs, levelDbfs: level });
      await new Promise<void>((resolve) => window.setTimeout(resolve, 50));
    }
  } finally {
    source.disconnect();
    stream.getTracks().forEach((track) => track.stop());
    await context.close();
  }
  const noiseFloorDbfs = percentile(quietLevels, 0.5);
  const speechP95Dbfs = percentile(speechLevels, 0.95);
  const peakDbfs = amplitudeDbfs(peak);
  const clippingRatio = total > 0 ? clipped / total : 0;
  const passed = speechP95Dbfs >= -45 && speechP95Dbfs - noiseFloorDbfs >= 12 && clippingRatio <= 0.01;
  const message = clippingRatio > 0.01
    ? "输入音量过高，请降低系统麦克风增益"
    : passed
      ? "麦克风收音正常"
      : "语音信号偏弱，请靠近麦克风或检查输入设备";
  return { passed, noiseFloorDbfs, speechP95Dbfs, peakDbfs, clippingRatio, sampleRate: context.sampleRate, message };
}
