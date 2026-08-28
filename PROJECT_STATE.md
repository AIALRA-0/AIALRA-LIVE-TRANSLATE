# AIALRA-LIVE-TRANSLATE 项目状态

## 1 状态摘要

状态日期：2026-08-27

当前版本：`0.2.0` 真实 GPU 混合链路

当前结论：`pivot` 已执行，浏览器课程工作台保留，默认 mock、同步模型请求和 VPS CPU 模型主路径已经移除

当前生产结构由受保护网页接收音频，Rust Core 先持久化并 ACK，再把模型任务写入 SQLite WAL，本机 RTX GPU Agent 通过私有出站连接领取任务

GPU 离线时不会生成占位字幕，音频继续安全保存并排队，GPU 恢复或 Core 重启后任务继续执行

## 2 本轮已通过门禁

- 本机真实 GPU 闭环：21／21 音频 ACK，4 条字幕，4 条译文，1 页材料，1 张讲解卡
- 私有远程 GPU 闭环：服务器持久队列经私有 Gateway 到 RTX 4080，真实 CUDA Provider 全部返回
- GPU 离线恢复：离线时 0 条假字幕与 5 个持久任务，恢复后 4 条字幕与 4 条译文，重复字幕为 0
- Core 重启恢复：5 个排队任务和 `processing` 会话状态保留，恢复后重复字幕为 0
- Provider 门禁：生产只接受 `faster-whisper:<model>@cuda`、`ollama:<model>@cuda` 和明确的本地文档解析器
- 浏览器：AudioWorklet、IndexedDB 未确认块、麦克风与共享音频入口、可见停止控制已接入
- 页面：黑白主视觉完成，红色仅用于录音、停止、拒绝与错误，桌面、暗色与 390 px 窄屏通过
- 线上入口：Authentik 保护、HTTPS/WSS、公开 `/internal/` 拒绝、私有 Gateway 单独监听均通过
- Windows Agent：DPAPI 令牌、登录自启动、健康检查、退避、心跳、租约与续租已接入

## 3 已完成范围

- Rust 状态机、追加式事件、SQLite WAL、内容寻址文件与持久模型队列
- ASR、翻译、讲解和材料任务的固定优先级、租约、重试与幂等完成
- 停止录音后的尾部音频封存与 `recording → processing → completed` 状态
- 浏览器 PCM 采集、序号、落盘后 ACK、重连、IndexedDB 恢复与来源标识
- Android 前台录音、原子缓存、断线重发、ACK 后清理与应用内停止
- `faster-whisper small` CUDA ASR 与 `qwen2.5:14b-instruct` CUDA 翻译和讲解
- PPTX、PDF、DOCX、图片、Markdown 和文本材料进入追加式时间线
- DingTalk A1 控制、服务器能力探针和小程序前台录音帧探针
- 共享 Authentik、Nginx、Cloudflare 入口和仅私有接口可达的 Worker Gateway
- 中英文 GitHub 首页与真实 GPU 合成样本截图

## 4 当前限制

- 30 分钟与 90 分钟真实模型稳定性门禁尚未执行，本轮显存数据只来自短时合成样本
- 真实英文课程、中英混说、多人、噪声、生僻术语和教师语速尚未形成系统基准
- 移动浏览器锁屏和后台录音受系统限制，长时移动收音仍优先 Android 前台服务或 DingTalk A1 并行录制
- Android 锁屏、耗电、来电、蓝牙与 Wi-Fi 切换尚未完成
- DingTalk 公开资料尚未证明第三方连续 PCM 或增量逐字稿能力
- 图片默认路径只保留原图与基础元数据，复杂 OCR、公式、图表和 VLM 尚未接入
- 多用户所有权、配额、保留期、删除、导出和备份界面尚未完成
- Windows 账户没有任务计划程序注册权限时，安装器退回当前用户启动项，尚未验证注销后冷启动
- 本轮完成 Core 与 GPU Agent 重启门禁，没有重启承载其他共享服务的整台主机

## 5 下一门禁

1. 使用同一公开合成音频执行 30 分钟连续真实链路，记录 ACK 丢失、p95、队列、显存与 OOM
2. 30 分钟通过后执行 90 分钟 Chrome、Edge 与 Android 前台真实收音
3. 使用登录用户麦克风完成一次 Authentik、权限、WSS、持久队列、私有 Gateway 与 GPU 的最终人工验收
4. 建立英语课程、中英混说、术语和噪声基准，比较 `small`、`large-v3` 与必要时的 7B／14B 模型组合
5. 接入 OCR 与页面视觉理解，再评估摄像头自动截屏与 DingTalk A1 会后对账
6. 邀请目标学生、教师与学校政策负责人复核许可流程、讲解密度和理解收益
