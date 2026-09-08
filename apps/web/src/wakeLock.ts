// One controller per recording page, including observers of a remote recorder.
export class RecordingWakeLock {
  private active = false;
  private pending = false;
  private sentinel: WakeLockSentinel | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  constructor(private readonly notify: (message: string) => void) {}

  setActive(active: boolean): void {
    this.active = active;
    if (active) void this.request();
    else {
      if (this.timer) clearTimeout(this.timer);
      this.timer = null;
      const sentinel = this.sentinel;
      this.sentinel = null;
      void sentinel?.release().catch(() => undefined);
    }
  }

  async request(): Promise<void> {
    if (!this.active || document.hidden || this.pending || this.sentinel) return;
    if (!navigator.wakeLock) {
      this.notify("浏览器不支持屏幕常亮，可在系统显示设置中延长自动锁屏时间");
      return;
    }
    this.pending = true;
    try {
      const sentinel = await navigator.wakeLock.request("screen");
      if (!this.active || document.hidden) {
        await sentinel.release();
        return;
      }
      this.sentinel = sentinel;
      this.notify("录音期间屏幕常亮，结束后自动恢复");
      sentinel.addEventListener("release", () => {
        if (this.sentinel !== sentinel) return;
        this.sentinel = null;
        if (this.active) {
          this.notify("系统释放了常亮，正在恢复");
          this.timer = setTimeout(() => void this.request(), 1000);
        }
      }, { once: true });
    } catch {
      if (this.active) this.notify("系统暂未允许常亮，请检查省电模式；点击可重试");
    } finally {
      this.pending = false;
    }
  }
}
