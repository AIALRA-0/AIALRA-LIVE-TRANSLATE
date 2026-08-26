import { frameWithHeader } from "./transport";

// ForegroundProbe verifies DingTalk RecorderManager frames and never claims lock-screen continuity.
export class ForegroundProbe {
  private readonly recorder = dd.getRecorderManager();
  private sequence = 1;
  private socketReady = false;

  constructor(
    private readonly websocketUrl: string,
    private readonly onStatus: (status: string) => void,
  ) {
    this.recorder.onFrameRecorded((frame) => this.sendFrame(frame));
    this.recorder.onError((error) => this.onStatus(error.errMsg || "录音帧读取失败"));
  }

  start(): void {
    dd.connectSocket({
      url: this.websocketUrl,
      success: () => {
        this.socketReady = true;
        this.onStatus("前台音频帧探针已连接");
        this.recorder.start({
          duration: 10 * 60 * 1_000,
          sampleRate: 16_000,
          numberOfChannels: 1,
          encodeBitRate: 256_000,
          format: "PCM",
          frameSize: 32,
        });
      },
      fail: (error) => this.onStatus(error.errorMessage || "WebSocket 连接失败"),
    });
  }

  stop(): void {
    this.recorder.stop();
    dd.closeSocket();
    this.socketReady = false;
    this.onStatus("前台音频帧探针已停止");
  }

  private sendFrame(frame: DingtalkRecorderFrame): void {
    if (!this.socketReady || frame.frameBuffer.byteLength === 0) return;
    const sequence = this.sequence;
    this.sequence += 1;
    dd.sendSocketMessage({
      data: frameWithHeader(sequence, Date.now(), frame.frameBuffer),
      fail: (error) => this.onStatus(error.errorMessage || `第 ${sequence} 帧发送失败`),
    });
    if (frame.isLastFrame) this.onStatus("钉钉已结束当前录音帧序列");
  }
}
