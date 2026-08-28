<div align="center">

# AIALRA-LIVE-TRANSLATE

浏览器直接收音，由受信任的本机 GPU 持续生成课程字幕、中文译文和证据可回溯的补充讲解

[English](README.en.md) · [部署说明](deploy/README.md) · [隐私边界](docs/PRIVACY_BOUNDARIES.md) · [验证记录](docs/VALIDATION_REPORT.md)

`Public beta` · `Local first` · `RTX CUDA verified` · `中文 / English`

![真实 RTX GPU 链路处理公开合成课程音频后的字幕与译文时间线](docs/assets/readme/real-gpu-timeline.png)

图 1　公开合成课程音频经 `faster-whisper:small@cuda` 和 `ollama:qwen2.5:14b-instruct@cuda` 处理后的真实页面，标识符已缩略，图片元数据已移除

</div>

## 它解决什么问题

听英文课程时，用户不应同时承担听写、翻译、生词查询、课件整理和笔记归档

AIALRA 把这些动作放进同一条课程时间线：

- 电脑或手机浏览器直接采集麦克风、标签页或系统共享音频
- 音频块先持久化再 ACK，模型离线不会阻塞录音和停止
- VPS 只负责入口、身份验证、持久队列和事件，RTX GPU Agent 主动领取任务
- 稳定字幕、译文、讲解和人工修订采用追加式事件，旧结果不会被静默覆盖
- PPT、PDF、DOCX、图片和文本进入后续讲解，并保留字幕或页码证据
- DingTalk A1 可作为同步录音和会后补偿来源，公开接口尚未证明连续第三方 PCM 能力

本项目不是隐蔽录音工具，真实录音前必须确认已经获得许可，并遵守课程、学校和适用法律要求

## 已验证的运行结构

```mermaid
flowchart TD
  Browser[电脑或手机浏览器] -->|HTTPS + WSS| Auth[Authentik 保护入口]
  Android[Android 前台录音] -->|WSS + ACK| Auth
  A1[DingTalk A1] -->|同步录制与会后补偿| Core
  Auth --> Core[Rust 音频与事件核心]
  Core --> Store[(SQLite WAL + 内容寻址文件)]
  Core --> Queue[(持久模型队列与租约)]
  Agent[Windows RTX GPU Agent] -->|Tailscale 私有领取| Queue
  Agent --> ASR[faster-whisper CUDA]
  Agent --> LLM[Ollama 14B CUDA]
```

音频接收、落盘、ACK 和停止控制不等待模型，GPU 离线时任务保持排队，不生成 identity 或 deterministic 占位结果

## 第一次本地运行

前置条件：Windows、Rust 1.95、Node.js 22、pnpm 10、Python 3.12 或 3.13、uv、Ollama 和 NVIDIA CUDA

先准备本地模型：

```powershell
ollama pull qwen2.5:14b-instruct
```

再启动完整本地链路：

```powershell
./scripts/start-local.ps1
```

脚本会构建网页、启动 Rust Core、模型 Worker 和双通道 GPU Agent，并只打开一个课程工作台页面

首次可观察结果：

1. 勾选“我已获得课程录音许可”
2. 选择麦克风或浏览器标签／系统共享音频
3. 点击“开始理解”，确认红色录音状态和音频 ACK 状态
4. 朗读或播放公开测试材料，等待字幕和译文显示实际 CUDA Provider
5. 点击“停止并保存”，界面会区分“录音已停止”和“模型处理完成”

浏览器实录需要 `localhost` 或 HTTPS 安全环境

## 当前能力

| 能力 | 当前状态 | 证据边界 |
|---|---|---|
| 浏览器收音 | 可用 | AudioWorklet、16 kHz 单声道 PCM、序号、ACK、IndexedDB 未确认块恢复 |
| 私有 GPU 链路 | 可用 | VPS 持久队列、60 秒租约、20 秒续租、Tailscale 私有 Gateway、DPAPI 令牌 |
| 实时字幕 | 可用 | `faster-whisper small + CUDA float16`，RTX 4080 已验证 |
| 中文翻译与讲解 | 可用 | `qwen2.5:14b-instruct` 本地 Ollama，结果必须证明 `@cuda` |
| 课程材料 | 可用 | PPTX、PDF、DOCX、图片和文本，复杂 OCR 与 VLM 仍在规划 |
| Android | 短时真机通过 | 前台服务、先落盘、ACK 后删除，90 分钟与锁屏门禁未完成 |
| DingTalk A1 | 控制与补偿链路 | 公开资料未证明第三方连续 PCM 或增量逐字稿能力 |
| 摄像头与自动截屏 | 规划中 | 默认关闭，尚未进入生产路径 |

## 真实门禁结果

测试条件：2026-08-27，Windows，RTX 4080 16 GB，公开合成英语课程音频，`small` ASR 和本地 14B 模型

| 门禁 | 结果 |
|---|---:|
| 本机完整链路 | 21／21 音频 ACK，4 字幕，4 译文，1 材料页，1 讲解卡，14.2 秒完成 |
| 本机 ASR 最慢窗口 | 1.89 秒 |
| 本机译文最慢段 | 0.87 秒 |
| 本机讲解 | 7.51 秒 |
| GPU 峰值观察 | 约 12.2 GB 显存，无 OOM |
| VPS → Tailscale → RTX 完整链路 | 21／21 音频 ACK，4 字幕，4 译文，1 材料页，1 讲解卡，35.0 秒完成 |
| GPU 离线 | 21／21 音频 ACK，0 假字幕，5 个任务持久排队 |
| GPU 恢复 | 4 字幕，4 译文，重复字幕 0 |
| Core 带队列重启 | 5 个任务和 `processing` 状态均保留，恢复后重复字幕 0 |
| 浏览器页面 | 桌面、暗色、390 px 窄屏通过，控制台错误与警告 0，横向溢出 0 |

这些数字只描述列出的合成样本和硬件，不代表所有口音、课堂噪声或长课程性能

复现入口见 [验证记录](docs/VALIDATION_REPORT.md) 和 `tools/e2e_smoke.mjs`

## 私有部署

混合部署由 VPS 持久接收音频，本机 GPU Agent 主动领取任务，VPS 不访问家庭公网地址

初始化 Windows DPAPI 令牌并安装登录自启动：

```powershell
./scripts/initialize-gpu-agent-secret.ps1
./scripts/install-gpu-agent.ps1 -GatewayUrl "http://worker-gateway.example.invalid"
```

VPS 只保存令牌 SHA-256 摘要，公开 Nginx 对 `/internal/` 固定拒绝，Worker Gateway 只绑定私有网络接口

完整部署和回滚边界见 [部署说明](deploy/README.md)

## 验证

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
pnpm lint
pnpm typecheck
pnpm test
pnpm build
uv run ruff check workers tools
uv run mypy workers tools
uv run pytest
```

## 数据与安全

- 录音、逐字稿、课件、令牌、私有地址和临时下载链接禁止进入 Git、测试快照和普通日志
- 真实录音必须先记录许可确认，并持续显示录音状态和停止入口
- 本地模式默认关闭云端文本与图片出口
- 原始数据位于独立私有目录，代码回滚不会删除用户会话
- 示例只使用合成内容、保留域名和空凭据

发现安全问题时，请使用 GitHub 的私密安全报告，不要在公开 Issue 附带录音、令牌、真实地址或服务器信息

## 项目目录

- `crates/`：Rust 状态机、事件、持久队列、音频接收和 API
- `workers/`：ASR、翻译、讲解、材料解析和 GPU Agent
- `apps/web/`：黑白单页课程工作台
- `apps/android/`：Android 长时录音客户端
- `apps/dingtalk-miniapp/`：DingTalk A1 控制与前台能力探针
- `deploy/`：VPS、Nginx、Authentik 和私有 Gateway 配置
- `docs/`：决策、研究、隐私、限制、变更和验证记录

## 支持与许可

问题和功能建议通过本仓库 Issue 跟踪

本仓库当前没有 `LICENSE` 文件，除适用法律另有规定外，权利人未授予复制、分发、修改或商用许可
