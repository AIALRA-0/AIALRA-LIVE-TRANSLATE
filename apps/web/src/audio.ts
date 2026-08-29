const TARGET_SAMPLE_RATE = 16_000;
const DATABASE_NAME = "aialra-audio-outbox";
const STORE_NAME = "frames";
const METADATA_STORE_NAME = "metadata";
const MAX_IN_FLIGHT_FRAMES = 8;

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

// AudioWorklet keeps capture off the UI thread while IndexedDB survives refreshes and short outages.
export class BrowserCapture {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private worklet: AudioWorkletNode | null = null;
  private socket: WebSocket | null = null;
  private sequence = 1;
  private pending = new Map<number, ArrayBuffer>();
  private inFlight = new Set<number>();
  private reconnectTimer: number | null = null;
  private renewTimer: number | null = null;
  private stopped = false;
  private stopping = false;
  private sampleChunks: Float32Array[] = [];
  private sampleCount = 0;
  private writeChain = Promise.resolve();
  private readonly sourceId: string;
  private readonly sequenceStorageKey: string;

  constructor(
    private readonly projectId: string,
    private readonly sessionId: string,
    private readonly recorderDeviceId: string,
    private readonly leaseToken: string,
    leaseGeneration: number,
    private readonly onStatus: (message: string) => void,
    private readonly mode: CaptureMode = "microphone",
    private readonly deviceId?: string,
    private readonly onRevoked?: () => void,
  ) {
    this.sourceId = `browser-${mode === "screen" ? "screen" : "mic"}-g${leaseGeneration}`;
    this.sequenceStorageKey = `aialra-audio-next-sequence:${sessionId}:${this.sourceId}`;
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
    const storedSequence = await loadNextSequence(this.sequenceStorageKey);
    this.sequence = recoverNextSequence(
      storedSequence,
      recovered.map((item) => item.sequence),
    );
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
      this.acceptSamples(event.data);
    };
    source.connect(this.worklet);
    this.worklet.connect(this.context.destination);
    this.connect();
    this.renewTimer = window.setInterval(() => void this.renewLease(), 10_000);
  }

  async stop(): Promise<void> {
    if (this.stopped) return;
    this.stopping = true;
    this.worklet?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    await this.context?.close();
    if (this.sampleCount > 0) {
      const tail = this.takeSamples(this.sampleCount);
      this.writeChain = this.writeChain.then(() => this.queueFrame(tail));
    }
    await this.writeChain;
    const deadline = Date.now() + 30_000;
    while (this.pending.size > 0 && Date.now() < deadline) {
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
    this.socket?.close();
    this.onStatus("录音已停止，已采集音频均得到服务器确认");
  }

  revoke(): void {
    this.stopped = true;
    this.stopping = false;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    if (this.renewTimer !== null) window.clearInterval(this.renewTimer);
    this.worklet?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    void this.context?.close();
    this.socket?.close();
    this.onStatus(`录音权限已由另一台设备接管，${this.pending.size} 个未确认音频块保留在本机`);
    this.onRevoked?.();
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
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    this.socket = new WebSocket(
      `${scheme}://${window.location.host}/api/v1/sessions/${this.sessionId}/sources/${this.sourceId}/audio`,
      ["aialra.audio.v1", `lease.${this.leaseToken}`],
    );
    this.socket.binaryType = "arraybuffer";
    this.socket.onopen = () => {
      this.inFlight.clear();
      this.onStatus(`${this.mode === "screen" ? "共享音频" : "麦克风"}已连接，等待服务器确认音频块`);
      this.sendPending();
    };
    this.socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as { type: string; sequence?: number; message?: string };
      if (message.type === "audio.ack" && typeof message.sequence === "number") {
        this.inFlight.delete(message.sequence);
        this.pending.delete(message.sequence);
        void deleteFrame(`${this.sessionId}:${this.sourceId}:${message.sequence}`);
        if (this.pending.size === 0) this.onStatus("收音正常，服务器已确认全部音频块");
        this.sendPending();
      } else if (message.type === "audio.error") {
        this.onStatus(message.message || "服务器拒绝了音频块，等待连接恢复");
        this.socket?.close();
      }
    };
    this.socket.onclose = () => {
      if (this.stopped) return;
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
    this.onStatus("录音权限已由另一台设备接管，本机停止采集，未确认块仍保留");
    this.revoke();
  }
}
