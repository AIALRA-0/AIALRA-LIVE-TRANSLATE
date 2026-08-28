const TARGET_SAMPLE_RATE = 16_000;
const DATABASE_NAME = "aialra-audio-outbox";
const STORE_NAME = "frames";

export type CaptureMode = "microphone" | "screen";

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

function openOutbox(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE_NAME, 1);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(STORE_NAME)) {
        const store = database.createObjectStore(STORE_NAME, { keyPath: "key" });
        store.createIndex("sessionSource", ["sessionId", "sourceId"]);
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

async function saveFrame(record: PersistedFrame): Promise<void> {
  const database = await openOutbox();
  try {
    const transaction = database.transaction(STORE_NAME, "readwrite");
    await transactionRequest(transaction.objectStore(STORE_NAME).put(record));
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

// AudioWorklet keeps capture off the UI thread while IndexedDB survives refreshes and short outages.
export class BrowserCapture {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private worklet: AudioWorkletNode | null = null;
  private socket: WebSocket | null = null;
  private sequence = 1;
  private pending = new Map<number, ArrayBuffer>();
  private reconnectTimer: number | null = null;
  private stopped = false;
  private readonly sourceId: string;

  constructor(
    private readonly sessionId: string,
    private readonly onStatus: (message: string) => void,
    private readonly mode: CaptureMode = "microphone",
    private readonly deviceId?: string,
  ) {
    this.sourceId = mode === "screen" ? "browser-screen" : "browser-mic";
  }

  async start(): Promise<void> {
    this.stopped = false;
    if (!window.isSecureContext && window.location.hostname !== "localhost") {
      throw new Error("浏览器录音需要 HTTPS 安全连接");
    }
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("当前浏览器不支持录音，请改用最新版 Chrome 或 Edge");
    }
    const recovered = await listFrames(this.sessionId, this.sourceId);
    recovered.forEach((item) => this.pending.set(item.sequence, item.frame));
    this.sequence = Math.max(0, ...recovered.map((item) => item.sequence)) + 1;
    this.stream = this.mode === "screen"
      ? await navigator.mediaDevices.getDisplayMedia({ audio: true, video: true })
      : await navigator.mediaDevices.getUserMedia({
          audio: {
            channelCount: 1,
            echoCancellation: true,
            noiseSuppression: true,
            autoGainControl: true,
            ...(this.deviceId ? { deviceId: { exact: this.deviceId } } : {}),
          },
        });
    if (this.mode === "screen") {
      this.stream.getVideoTracks().forEach((track) => track.stop());
      if (this.stream.getAudioTracks().length === 0) {
        this.stream.getTracks().forEach((track) => track.stop());
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
    const source = this.context.createMediaStreamSource(this.stream);
    this.worklet = new AudioWorkletNode(this.context, "aialra-pcm");
    this.worklet.port.onmessage = (event: MessageEvent<Float32Array>) => {
      void this.queueSamples(event.data);
    };
    source.connect(this.worklet);
    this.worklet.connect(this.context.destination);
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    this.worklet?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    void this.context?.close();
    this.socket?.close();
    this.onStatus("录音已停止");
  }

  private async queueSamples(samples: Float32Array): Promise<void> {
    if (this.stopped) return;
    const resampled = resample(samples, this.context?.sampleRate ?? 48_000);
    const sequence = this.sequence;
    this.sequence += 1;
    const frame = encodeFrame(sequence, Date.now(), resampled);
    const key = `${this.sessionId}:${this.sourceId}:${sequence}`;
    await saveFrame({ key, sessionId: this.sessionId, sourceId: this.sourceId, sequence, frame });
    this.pending.set(sequence, frame);
    this.sendPending();
  }

  private connect(): void {
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    this.socket = new WebSocket(
      `${scheme}://${window.location.host}/api/v1/sessions/${this.sessionId}/sources/${this.sourceId}/audio`,
    );
    this.socket.binaryType = "arraybuffer";
    this.socket.onopen = () => {
      this.onStatus(`${this.mode === "screen" ? "共享音频" : "麦克风"}已连接，等待服务器确认音频块`);
      this.sendPending();
    };
    this.socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as { type: string; sequence?: number };
      if (message.type === "audio.ack" && typeof message.sequence === "number") {
        this.pending.delete(message.sequence);
        void deleteFrame(`${this.sessionId}:${this.sourceId}:${message.sequence}`);
        if (this.pending.size === 0) this.onStatus("收音正常，服务器已确认全部音频块");
      }
    };
    this.socket.onclose = () => {
      if (this.stopped) return;
      this.onStatus(`连接恢复中，${this.pending.size} 个音频块已安全缓存`);
      this.reconnectTimer = window.setTimeout(() => this.connect(), 1_500);
    };
  }

  private sendPending(): void {
    if (this.socket?.readyState !== WebSocket.OPEN) return;
    this.pending.forEach((frame) => this.socket?.send(frame));
  }
}
