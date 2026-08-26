// A1 control results stay explicit because a successful command does not imply live PCM access.
export async function startA1Recording(
  sessionId: string,
  templateId?: string,
): Promise<Record<string, unknown>> {
  return await new Promise((resolve, reject) => {
    dd.startDingerRecord({
      businessOrder: sessionId,
      templateId,
      success: resolve,
      fail: (error) => reject(new Error(error.errorMessage || "A1 录音启动失败")),
    });
  });
}

// Device status confirms connection and recording mode while preserving the documented status values.
export async function readA1Status(): Promise<DingerDeviceStatus> {
  return await new Promise((resolve, reject) => {
    dd.getDingerDeviceStatus({
      success: resolve,
      fail: (error) => reject(new Error(error.errorMessage || "无法读取 A1 状态")),
    });
  });
}
