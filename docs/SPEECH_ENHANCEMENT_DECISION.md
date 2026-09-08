# 录音增强与实时翻译：本批选择依据

本批先修输入与调度，再使用成熟组件改善噪声。没有证据证明一种算法在所有口音、房间、麦克风上都最好；也不能把降噪当作翻译语义错误的修复。

## 降噪方案比较

| 方案 | 质量与适用场景 | 浏览器、延迟与资源 | 成本与本批决定 |
|---|---|---|---|
| 浏览器原生音频处理 | 回声消除和基本背景噪声；实际实现取决于浏览器 | 原生执行，无额外模型下载；效果需按设备验证 | 无额外授权费用，保留为后备选项 |
| RNNoise | 成熟的 DSP 与神经网络混合语音降噪；适合连续背景噪声，不保证保留所有弱音 | 48 kHz；通过现成 AudioWorklet/WASM 组件本地运行。本批普通/SIMD WASM 各约 153/157 kB，实际只按能力加载一种 | RNNoise BSD-3-Clause、封装 MIT；本批默认集成，允许关闭，不与浏览器降噪叠加 |
| DeepFilterNet | 全频带神经语音增强，适合作为更高质量候选 | 官方主要面向原生运行；当前项目需要额外浏览器适配与实测，不能直接断言更快 | 开源路线；本批不再搭建第二套音频运行时 |
| Krisp Web SDK | 商业语音增强，适合采购后做同输入对照 | 提供浏览器 SDK 和能力检查；官方说明了缓冲区与 flush 配置，需测实际设备延迟 | 需商业授权与报价；没有授权和同条件测量，不宣称已经比较实测质量或集成 |

参考：[RNNoise 官方实现](https://github.com/xiph/rnnoise)、[浏览器组件](https://github.com/sapphi-red/web-noise-suppressor)、[DeepFilterNet](https://github.com/Rikorose/DeepFilterNet)、[Krisp Web SDK](https://sdk-docs.krisp.ai/docs/getting-started-js)。以上是工程选型，不是商业产品效果排名。

清晰的在线视频优先使用浏览器共享标签页音频，避免扬声器、房间、麦克风和回声消除二次改变信号。共享音源默认不额外降噪。麦克风测试只判断输入电平和削波，不把音量读数当作识别准确率。

## 翻译与实时框架

保持用户已认可的 Qwen3-ASR，不因翻译问题重做 ASR。HY-MT 使用官方专用模板，不再使用通用聊天 system 消息。公开课程固定 ASR 对照发现自由历史会把前文数量带入当前译文，因此当前翻译只接收完整源段落和显式术语；兼容接收的历史上下文不再拼入提示词。限制解码时间和输出长度，截断结果不得冒充完整译文。[HY-MT 官方说明](https://huggingface.co/tencent/HY-MT1.5-1.8B)

采用成熟实时评估的做法，分别记录音频输入时长、排队时间、推理时间、稳定字幕和译文到达时间；不能用一次性快速灌入音频的总耗时冒充实时延迟。参考 [SimulEval](https://github.com/facebookresearch/SimulEval) 的质量与延迟分开评价方式、[SimulStreaming](https://github.com/ufal/SimulStreaming) 的增量稳定输出设计。本批复用这些原则，不把未经验证的新 ASR 框架直接替换到生产。

比较输入至少包含否定、条件、数字单位、技术名词和语言切换。声学增强后还应比较识别漏词，不能只看声音是否更干净。可使用 [DNS Challenge](https://github.com/microsoft/DNS-Challenge) 的成对噪声评价方法；尚无本项目的完整 MOS/商业同场对照证据。

## 可验证边界

- 真实 Chromium 验证本地 WASM 加载、音频 ACK、停止释放、设备互斥和窄屏布局。
- GPU 超时后的实际推理保持独占；忙碌请求在原任务租约内等待，避免并发重试叠加。看门狗仅恢复本项目进程，不终止其他产品。
- 屏幕常亮只在录音期间申请，停止或离开后释放；系统可能因锁屏、省电或页面隐藏撤销授权，网页不能绕过操作系统限制。[Screen Wake Lock](https://developer.mozilla.org/en-US/docs/Web/API/Screen_Wake_Lock_API)
- 已取得用户指定视频的 05:00–06:00 音轨，在仓库外进行原声、加噪／混响和 RNNoise 对比。真实 Chromium 实时运行正常；离线 AudioContext 试验未等待异步 WASM 初始化，产生空输出，已判为无效方法并舍弃其结果。噪声模拟不是用户手机和房间的实测；没有人工校对参考或商业产品同输入对照，不宣称全面达到钉钉等产品效果。
