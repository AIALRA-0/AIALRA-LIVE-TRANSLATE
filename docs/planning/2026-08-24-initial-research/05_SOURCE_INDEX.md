# AIALRA-LIVE-TRANSLATE 调查来源索引

访问日期：2026-08-24

使用原则：优先使用官方开放平台、官方项目仓库、官方产品页、现有 AIALRA 仓库和用户文件库；产品宣称只用于说明公开功能，不直接视为独立质量证明

## 1 引用表

### 1.1 [1] DingTalk `startDingerRecord`

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/jsapi-start-dinger-record
- 支持结论：A1 录音控制和自定义 `businessOrder`

### 1.2 [2] DingTalk `RecorderManager.onFrameRecorded`

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/jsapi-recorder-manager-on-frame-recorded
- 支持结论：小程序可以接收分帧录音数据

### 1.3 [3] 根据听记 ID 获取 A1 音频文件信息

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/api-queryfileinfobyminutesid
- 支持结论：会后听记记录可以映射到音频文件信息

### 1.4 [4] 分页查询指定设备的音频文件列表

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/api-queryaudiofile
- 支持结论：可以按设备发现录音文件

### 1.5 [5] 查询 DingTalk A1 小助理分析

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/api-querysmartdeviceaisummary
- 支持结论：可以查询 A1 官方分析结果

### 1.6 [6] DingTalk A1 小助理总结完成事件

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/events-aone-assistant-summary-change
- 支持结论：分析完成可以通过事件通知

### 1.7 [7] DingTalk `getDingerDeviceStatus`

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/jsapi-get-dinger-device-status
- 支持结论：可以查询 A1 设备状态

### 1.8 [8] DingTalk `RecorderManager.start`

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/isvapp/jsapi-recorder-manager-start
- 支持结论：官方说明页面不可见时录音自动停止，是长课程小程序路线的关键限制

### 1.9 [9] DingTalk `sendSocketMessage`

- 类型：官方开放平台
- 链接：https://open.dingtalk.com/document/development/jsapi-send-socket-message
- 支持结论：小程序在建立 WebSocket 后可以发送数据

### 1.10 [10] AIALRA Human-Readable Technical Writing Skill

- 类型：用户 GitHub 仓库
- 链接：https://github.com/AIALRA-0/agent-human-readable-technical-writing
- 支持结论：报告结构、证据链、术语解释、数字来源和代理交付规则

### 1.11 [11] `BabbleDeck_Full_Development_Package.md`

- 类型：用户文件库中的内部设计包
- 文件：`BabbleDeck_Full_Development_Package.md`
- 支持结论：早期实时音频、事件、恢复、数据库、测试和 Codex 工作流设计
- 边界：该文件是历史设计输入，不能代替当前仓库审计

### 1.12 [12] BabbleDeck 当前仓库 `PROJECT_MEMORY.md`

- 类型：用户 GitHub 仓库
- 链接：https://github.com/AIALRA-0/BabbleDeck
- 支持结论：现有生产平台已经覆盖 WebSocket、SSE、Soniox、音频备份、事件、导出、Tauri、Capacitor、LiveKit 和生产运维

### 1.13 [13] AIALRA Documentation Style

- 类型：用户 GitHub 仓库
- 链接：https://github.com/AIALRA-0/AIALRA-DOCUMENTATION-STYLE
- 支持结论：文档结构、多人协作、更新和格式规范

### 1.14 [14] faster-whisper

- 类型：官方项目仓库
- 链接：https://github.com/SYSTRAN/faster-whisper
- 发布页：https://github.com/SYSTRAN/faster-whisper/releases
- 支持结论：CTranslate2 Whisper 实现、`large-v3-turbo`、批处理和 VAD 相关能力

### 1.15 [15] FunASR

- 类型：官方项目仓库
- 链接：https://github.com/modelscope/FunASR
- 支持结论：离线、流式和边缘 ASR，VAD、标点、说话人和多模型管线
- 许可证边界：工具代码和具体模型权重需要分别审计

### 1.16 [16] NVIDIA NeMo Speech Model Selection

- 类型：官方文档
- 链接：https://docs.nvidia.com/nemo/speech/nightly/starthere/choosing_a_model.html
- 支持结论：Nemotron Streaming、Parakeet、Canary 和 Sortformer 等候选的官方定位

### 1.17 [17] Silero VAD

- 类型：官方项目仓库
- 链接：https://github.com/snakers4/silero-vad
- 支持结论：轻量 CPU 语音活动检测和 8 kHz、16 kHz 支持

### 1.18 [18] WhisperLiveKit

- 类型：开源项目仓库
- 链接：https://github.com/QuentinFuxa/WhisperLiveKit
- 支持结论：本地实时 ASR、说话人、翻译和兼容 API 的原型参考

### 1.19 [19] SimulStreaming

- 类型：研究开源项目
- 链接：https://github.com/ufal/SimulStreaming
- 支持结论：Whisper 加 LLM 翻译的增量长语音处理和术语上下文设计

### 1.20 [20] Qwen3-VL

- 类型：官方项目仓库
- 链接：https://github.com/QwenLM/Qwen3-VL
- 支持结论：2B、4B、8B 等本地视觉模型、OCR、文档和视频能力

### 1.21 [21] Docling

- 类型：官方文档
- 链接：https://docling-project.github.io/docling/
- 支持结论：PDF、DOCX、PPTX、图片、音频等格式的本地解析和统一文档表示

### 1.22 [22] python-pptx

- 类型：官方文档
- 链接：https://python-pptx.readthedocs.io/
- 支持结论：PPTX 文本、图片、表格、备注和页面结构提取

### 1.23 [23] Seamless Communication

- 类型：官方研究项目仓库
- 链接：https://github.com/facebookresearch/seamless_communication
- 支持结论：SeamlessStreaming 的语音输入和文本或语音输出翻译路线

### 1.24 [24] DeepSeek API Change Log

- 类型：官方 API 文档
- 链接：https://api-docs.deepseek.com/updates/
- 支持结论：V4 Flash、V4 Pro、思考档位和版本更新

### 1.25 [25] DeepSeek Models and Pricing

- 类型：官方 API 文档
- 链接：https://api-docs.deepseek.com/quick_start/pricing
- 中文页：https://api-docs.deepseek.com/zh-cn/quick_start/pricing/
- 支持结论：当前模型能力和按 token 价格；价格可能调整

### 1.26 [26] DeepSeek V4 Flash Vision

- 类型：官方 API 文档
- 链接：https://api-docs.deepseek.com/guides/vision/
- 发布说明：https://api-docs.deepseek.com/news/news260821/
- 支持结论：实验视觉模型接受图片和文本

### 1.27 [27] DeepSeek 模型发现 Codex 集成

- 类型：官方 API 文档
- 模型列表：https://api-docs.deepseek.com/api/list-models/
- Codex 集成：https://api-docs.deepseek.com/quick_start/agent_integrations/codex/
- 支持结论：运行时发现可用模型，避免把模型名永久写死

### 1.28 [28] Qwen3 本地部署

- 类型：官方博客
- 链接：https://qwenlm.github.io/blog/qwen3/
- 支持结论：vLLM、SGLang、Ollama、LM Studio、llama.cpp 等本地部署路线

### 1.29 [29] 本地嵌入候选

- 类型：官方项目仓库
- Qwen3 Embedding：https://github.com/QwenLM/Qwen3-Embedding
- FlagEmbedding 和 BGE-M3：https://github.com/FlagOpen/FlagEmbedding
- 支持结论：中英和多语言文本检索、小型嵌入模型和长文本支持

### 1.30 [30] 本地 TTS 候选

- 类型：官方或主要项目仓库
- Kokoro：https://github.com/hexgrad/kokoro
- CosyVoice：https://github.com/FunAudioLLM/CosyVoice
- Qwen3-TTS：https://github.com/QwenLM/Qwen3-TTS
- 支持结论：轻量和中英语音合成的后续候选

### 1.31 [31] 开源架构参考合集

- 类型：开源项目
- Meetily：https://github.com/Zackriya-Solutions/meetily
- Vexa：https://github.com/vexa-ai/vexa
- screenpipe：https://github.com/screenpipe/screenpipe
- WhisperLiveKit：https://github.com/QuentinFuxa/WhisperLiveKit
- 支持结论：本地桌面捕获、会议机器人、屏幕时间线、实时 ASR 和开放 API 的工程参考
- 许可证边界：screenpipe 使用 source-available 商业许可证，不能按普通开源代码直接复用

### 1.32 [32] DingTalk A1 产品说明

- 类型：DingTalk 官方全球站产品说明
- 链接：https://www.dingtalk-global.com/news/explain/how-dingtalk-a1-transforms-voice-into-action-26010551
- 支持结论：产品侧实时转写和设备能力
- 边界：产品侧能力不等于开放平台实时 API

### 1.33 [33] PLAUD Note Pro

- 类型：官方产品页
- 链接：https://www.plaud.ai/products/plaud-note-pro
- 支持结论：多语言转写、说话人标签、自定义词汇和总结

### 1.34 [34] Notta Memo

- 类型：官方产品页
- 链接：https://www.notta.ai/en/hardware/memo
- 支持结论：录音、实时转写、翻译和总结硬件路线

### 1.35 [35] 讯飞听见

- 类型：官方产品页和帮助页
- 链接：https://www.iflyrec.com/
- 实时翻译说明：https://www.iflyrec.com/helpCenter_guide/helpCenter_guide.html
- 支持结论：中文场景的实时转写、翻译、悬浮字幕和边录边拍照

### 1.36 [36] Otter Meeting Agent

- 类型：官方产品页
- 链接：https://otter.ai/
- 教育页：https://otter.ai/education
- 支持结论：实时转写、摘要、AI Chat 和教育场景

### 1.37 [37] Otter Automated Slide Capture

- 类型：官方帮助页
- 链接：https://help.otter.ai/hc/en-us/articles/5093321813911-Automated-Slide-Capture-Overview
- 支持结论：线上会议自动抓取幻灯片和屏幕共享

### 1.38 [38] Granola

- 类型：官方产品页
- 链接：https://www.granola.ai/
- 支持结论：设备音频、无机器人会议笔记和移动端现场记录

### 1.39 [39] Krisp AI Meeting Assistant

- 类型：官方产品页
- 链接：https://krisp.ai/ai-meeting-assistant/
- 移动端说明：https://help.krisp.ai/hc/en-us/articles/20282426717596-Krisp-mobile-app-record-and-transcribe-in-person-meetings
- 支持结论：跨会议应用和现场录音、转写与总结

### 1.40 [40] Fireflies

- 类型：官方产品页
- 链接：https://fireflies.ai/
- 语言说明：https://guide.fireflies.ai/articles/2973706448-learn-about-fireflies-supported-languages
- 支持结论：会议转写、说话人、总结和多语言

### 1.41 [41] Notta 软件平台

- 类型：官方产品页
- 链接：https://www.notta.ai/en
- 实时转写：https://www.notta.ai/en/real-time-transcription
- 支持结论：讲座、会议、搜索、实时转写和总结工作流

### 1.42 [42] Wordly

- 类型：官方产品页
- 链接：https://www.wordly.ai/
- 支持结论：现场和线上实时翻译、字幕、逐字稿、总结和词汇表

### 1.43 [43] KUDO Glossary Management

- 类型：官方产品新闻
- 链接：https://kudo.ai/newsroom/press-release/kudo-launches-client-glossary-management-for-greater-ai-customization-and-terminology-accuracy/
- 支持结论：Word Priority 和 Do Not Translate 术语控制

### 1.44 [44] Interprefy

- 类型：官方产品页
- 链接：https://www.interprefy.com/
- AI 翻译：https://www.interprefy.com/solutions/ai-translation
- 支持结论：现场和线上实时 AI 语音翻译与字幕

### 1.45 [45] Soniox 实时语音翻译

- 类型：官方开发文档
- 实时转写：https://soniox.com/docs/stt/rt/real-time-transcription
- 实时翻译：https://soniox.com/docs/translation/stt-translation/rt-translation
- 模型：https://soniox.com/docs/stt/models
- 支持结论：同一 WebSocket 中增量输出转写和译文，是 BabbleDeck 现有云端路线的重要参考

### 1.46 [46] Meetily

- 类型：开源项目仓库
- 链接：https://github.com/Zackriya-Solutions/meetily
- 支持结论：Rust、本地录音、Whisper 或 Parakeet 和 Ollama 总结的本地会议助手

### 1.47 [47] Vexa

- 类型：开源项目仓库
- 链接：https://github.com/vexa-ai/vexa
- 支持结论：自托管会议机器人、实时 WebSocket 逐字稿、API 和 Agent 工作流

### 1.48 [48] screenpipe

- 类型：source-available 项目仓库
- 链接：https://github.com/screenpipe/screenpipe
- 支持结论：本地连续屏幕和音频时间线
- 许可证边界：商业复用前必须检查其商业许可证

### 1.49 [49] WhisperLiveKit

- 类型：开源项目仓库
- 链接：https://github.com/QuentinFuxa/WhisperLiveKit
- 支持结论：实时本地 ASR、说话人、翻译和兼容 API，可作为基准和原型参考

### 1.50 [50] California Penal Code Section 632

- 类型：加州立法机关官方法规页
- 链接：https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PEN&sectionNum=632
- 支持结论：对具有保密期待的交流录音设置同意要求及相关边界
- 边界：本报告不构成法律意见

### 1.51 [51] USC Course Recording Policy Example

- 类型：USC 课程政策公开页
- 链接：https://aste-classes.usc.edu/courses/?course=ASTE101
- 支持结论：未经教师明确许可并向全班说明，不得录制课程；课程内容不得擅自分发

## 2 资料更新规则

- DingTalk、DeepSeek、Qwen、Soniox、竞品能力和价格具有时效性，Codex 在实现 Provider 前重新读取官方文档
- GitHub 开源项目在复用代码前读取当前 LICENSE、NOTICE 和模型卡
- A1 真机结果优先于产品宣传，真实接口响应优先于二手文章
- 性能和显存结论只能来自目标 RTX 4080 的可复现基准
