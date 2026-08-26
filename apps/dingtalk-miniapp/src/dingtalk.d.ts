// DingTalk declarations cover only the official JSAPIs exercised by this foreground probe.
interface DingtalkRecorderFrame {
  frameBuffer: ArrayBuffer;
  isLastFrame: boolean;
}

interface DingtalkRecorderManager {
  start(options: {
    duration: number;
    sampleRate: number;
    numberOfChannels: number;
    encodeBitRate: number;
    format: "PCM";
    frameSize: number;
  }): void;
  stop(): void;
  onFrameRecorded(callback: (frame: DingtalkRecorderFrame) => void): void;
  onError(callback: (error: { errMsg?: string }) => void): void;
}

interface DingtalkApi {
  getRecorderManager(): DingtalkRecorderManager;
  connectSocket(options: {
    url: string;
    success?: () => void;
    fail?: (error: { errorMessage?: string }) => void;
  }): void;
  sendSocketMessage(options: {
    data: ArrayBuffer;
    success?: () => void;
    fail?: (error: { errorMessage?: string }) => void;
  }): void;
  closeSocket(): void;
  startDingerRecord(options: {
    businessOrder: string;
    templateId?: string;
    success?: (result: Record<string, unknown>) => void;
    fail?: (error: { errorMessage?: string }) => void;
  }): void;
  getDingerDeviceStatus(options: {
    success?: (result: DingerDeviceStatus) => void;
    fail?: (error: { errorMessage?: string }) => void;
  }): void;
}

interface DingerDeviceStatus {
  audio_status?: "idle" | "recording" | "paused" | "streaming" | "rec_streaming";
  device_id?: string;
  device_sn?: string;
  firmware_version?: string;
  battery_level?: number;
  storage_available?: number;
}

declare const dd: DingtalkApi;
