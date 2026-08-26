const TARGET_SAMPLE_RATE = 16_000;

// Big-endian 64-bit fields match the Rust and Android transport contract.
function writeU64(view: DataView, offset: number, value: number): void {
  view.setBigUint64(offset, BigInt(Math.max(0, Math.floor(value))), false);
}

// Linear resampling is sufficient for the transport bootstrap and is measured against server-side ASR.
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

// PCM conversion clips microphone peaks and prepends the reliable transport header.
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

// BrowserCapture keeps unacknowledged frames in memory and resends them after a reconnect.
export class BrowserCapture {
  private context: AudioContext | null = null;
  private stream: MediaStream | null = null;
  private processor: ScriptProcessorNode | null = null;
  private socket: WebSocket | null = null;
  private sequence = 1;
  private pending = new Map<number, ArrayBuffer>();
  private reconnectTimer: number | null = null;
  private stopped = false;

  constructor(
    private readonly sessionId: string,
    private readonly onStatus: (message: string) => void,
    private readonly deviceId?: string,
  ) {}

  async start(): Promise<void> {
    this.stopped = false;
    if (!window.isSecureContext && window.location.hostname !== "localhost") {
      throw new Error("浏览器录音需要 HTTPS 安全连接");
    }
    if (!navigator.mediaDevices?.getUserMedia) {
      throw new Error("当前浏览器不支持麦克风录音，请改用最新版 Chrome、Edge 或 Safari");
    }
    this.stream = await navigator.mediaDevices.getUserMedia({
      audio: {
        channelCount: 1,
        echoCancellation: true,
        noiseSuppression: true,
        autoGainControl: true,
        ...(this.deviceId ? { deviceId: { exact: this.deviceId } } : {}),
      },
    });
    this.context = new AudioContext({ latencyHint: "interactive" });
    const source = this.context.createMediaStreamSource(this.stream);
    // ScriptProcessor remains widely available for the first local slice; AudioWorklet replaces it later.
    this.processor = this.context.createScriptProcessor(4096, 1, 1);
    this.processor.onaudioprocess = (event) => {
      const samples = resample(event.inputBuffer.getChannelData(0), this.context?.sampleRate ?? 48_000);
      const sequence = this.sequence;
      this.sequence += 1;
      const frame = encodeFrame(sequence, Date.now(), samples);
      this.pending.set(sequence, frame);
      this.sendPending();
    };
    source.connect(this.processor);
    this.processor.connect(this.context.destination);
    this.connect();
  }

  stop(): void {
    this.stopped = true;
    if (this.reconnectTimer !== null) window.clearTimeout(this.reconnectTimer);
    this.processor?.disconnect();
    this.stream?.getTracks().forEach((track) => track.stop());
    void this.context?.close();
    this.socket?.close();
    this.onStatus("麦克风已停止");
  }

  private connect(): void {
    const scheme = window.location.protocol === "https:" ? "wss" : "ws";
    this.socket = new WebSocket(
      `${scheme}://${window.location.host}/api/v1/sessions/${this.sessionId}/sources/browser/audio`,
    );
    this.socket.binaryType = "arraybuffer";
    this.socket.onopen = () => {
      this.onStatus("麦克风已连接，音频块正在发送并等待服务器确认");
      this.sendPending();
    };
    this.socket.onmessage = (event) => {
      const message = JSON.parse(String(event.data)) as { type: string; sequence?: number };
      if (message.type === "audio.ack" && typeof message.sequence === "number") {
        this.pending.delete(message.sequence);
        if (this.pending.size === 0) {
          this.onStatus("收音正常，服务器已确认全部音频块");
        }
      }
    };
    this.socket.onclose = () => {
      if (this.stopped) return;
      this.onStatus(`连接恢复中，${this.pending.size} 个音频块等待确认`);
      this.reconnectTimer = window.setTimeout(() => this.connect(), 1_500);
    };
  }

  private sendPending(): void {
    if (this.socket?.readyState !== WebSocket.OPEN) return;
    this.pending.forEach((frame) => this.socket?.send(frame));
  }
}
