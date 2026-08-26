# Codex Bootstrap Prompt — AIALRA-LIVE-TRANSLATE

你是 Codex，负责在用户本机长期、增量、可验证地端到端开发 `AIALRA-LIVE-TRANSLATE`

本提示词是首轮执行指令，不是仅供讨论的建议

## 1 任务目标

构建一个本地优先的多模态实时学习副驾

系统需要接收 Android 手机、电脑麦克风、DingTalk A1 会后文件、钉钉小程序实验流、音频文件、PPTX、PDF、图片和后续摄像头或屏幕内容

系统需要在本地完成或优先完成

- 实时语音识别
- 原文和译文双语字幕
- 临时字幕和稳定字幕分离
- 术语表和专有名词修复
- 周期摘要、缺漏补充和生僻词一句话解释
- 用户中途上传材料后，在下一次解释中引用相关页面
- 所有内容进入可回放、可修订和可导出的统一时间线

目标硬件

```text
CPU: Intel Core i9-13900K
GPU: NVIDIA RTX 4080 16 GB
RAM: 64 GB
Default deployment: local Windows machine
```

## 2 先读文件

在修改任何代码前，按顺序读取

1. 仓库根目录的 `AGENTS.md`
2. 仓库根目录的 `PROJECT_STATE.md`、`PROJECT_MEMORY.md` 或同类状态文件
3. 仓库 README、架构文档、ADR、已知问题和测试记录
4. 本研究包的 `01_INITIAL_RESEARCH_REPORT.md`
5. 本研究包的 `03_IMPLEMENTATION_BACKLOG.md`
6. 本研究包的 `04_EXECUTION_MANIFEST.yaml`
7. AIALRA 本地可用的 `agent-human-readable-technical-writing` Skill
8. AIALRA 本地可用的 `AIALRA-DOCUMENTATION-STYLE` 规范

若研究包尚未复制进仓库，先把它复制到 `docs/planning/2026-08-24-initial-research/`，保留原文件名和内容

## 3 第一原则

### 3.1 先审计，后修改

当前对话无法访问用户本机仓库，因此你不能假设仓库为空，也不能假设报告中的目标结构已经存在

第一轮必须先做只读审计

审计内容

- 当前路径和 Git 根目录
- 当前分支、远端和最近提交
- 工作区是否干净
- 未提交、未跟踪和忽略文件
- 文件树和主要语言
- 现有进程、端口和本地服务
- 当前构建、测试和启动命令
- 当前数据库、模型、缓存和数据目录
- 当前协议和事件类型
- 当前 Android、Tauri、Web、Rust 和 Python 代码
- 当前 DingTalk 或 A1 集成
- 与 BabbleDeck 的复制、依赖、协议或集成关系
- 当前用户真实需求已经完成到什么程度

把结果写入

```text
docs/audits/INITIAL_REPO_AUDIT.md
```

审计报告必须区分

- 已实现且已验证
- 已实现但未验证
- 只有文档或占位
- 完全缺失
- 与本研究包冲突
- 可以复用 BabbleDeck
- 需要用户凭据或真机才能验证

### 3.2 只增量，不破坏

- 不覆盖用户现有未提交修改
- 不删除现有功能来迎合本报告
- 不重建 Git 历史
- 不执行破坏性数据库操作
- 不删除原始录音、模型和用户数据
- 不把实验结果写进正式数据目录
- 不提交密钥、Cookie、设备令牌、A1 临时 URL、私人录音或课程材料
- 重大架构调整使用新分支和 ADR
- 发现冲突时优先适配现有成熟实现

### 3.3 不虚报完成

以下情况不能写“已完成”

- 只编译通过，没有运行真实页面
- 只用 Mock，没有运行真实模型
- 只调用 A1 文档示例，没有真机结果
- 只上传文件，没有验证下一次解释确实引用它
- 只生成字幕，没有完成断网恢复
- 只执行一次短音频，没有完成长时测试
- 只看到进程存活，没有验证用户流程

每个报告都要区分全局目标完成度和当前本地任务完成度

## 4 不可变架构边界

除非通过 ADR 和证据证明需要改变，否则遵守以下决定

### 4.1 ADR-001

A1 实时音频或实时逐字稿未由公开接口证实，不能进入 MVP 唯一关键路径

A1 首版负责

- 启停和状态
- `businessOrder` 关联
- 会后文件发现和导入
- 听记 ID
- 官方分析和总结完成事件
- 与本地结果对账

A1 实时能力只能放在独立探针中验证

### 4.2 ADR-002

核心领域、状态机、事件、传输、持久化和调度优先使用 Rust

模型和文档生态使用 Python Worker

前端使用 React 和 TypeScript

Android 长时录音使用 Kotlin 原生

### 4.3 ADR-003

本地单用户默认使用 SQLite WAL 和本地对象目录

不为首版引入 Kafka、Redis、分布式数据库或强制云对象存储

### 4.4 ADR-004

Android 原生 ForegroundService 是手机长时录音主链路

钉钉小程序只用于前台短时实验、A1 控制和能力探针

### 4.5 ADR-005

片段是主时间线

临时 ASR、稳定片段、片段修订、翻译、翻译修订、解释、资产和对齐均通过追加事件记录

禁止静默覆盖历史

### 4.6 ADR-006

字幕拥有最高 GPU 资源优先级

解释、视觉、说话人分离、深度总结和 TTS 必须支持取消、延迟或降级

### 4.7 ADR-007

PPT、PDF 和图片必须成为有稳定 ID 的资产和页面，不能只作为聊天附件

解释结果必须引用字幕片段 ID 和页面 ID

### 4.8 ADR-008

默认 `local_only`

云端文本或多模态调用必须经过策略引擎和会话级用户授权

## 5 BabbleDeck 复用边界

用户已有 `AIALRA-0/BabbleDeck`

该项目已经具备浏览器录音、WebSocket、SSE、Soniox、事件持久化、术语表、音频恢复、导出、LiveKit、Tauri、Capacitor、Playwright 和生产运维

你必须先审计本机是否已经克隆 BabbleDeck，并比较以下对象

- 会话和事件 Schema
- 音频块协议
- Provider 接口
- Viewer 和字幕组件
- 术语表
- 导出
- 测试夹具
- Tauri 和 Android 外壳

优先选择

- 抽取共享协议包
- 通过适配器调用 BabbleDeck
- 复用测试模式和 UI 组件
- 保持两个产品职责清晰

禁止在没有审计和 ADR 的情况下复制整套 BabbleDeck 或把其生产服务直接重写为 Rust

## 6 首轮执行顺序

### 6.1 Step 0 保护现场

执行并记录

```bash
git status --short --branch
git remote -v
git log -10 --oneline --decorate
```

若存在未提交修改

- 不 stash
- 不 reset
- 不 checkout 覆盖文件
- 在审计中列出
- 只修改与用户工作不冲突的新文件或新分支

### 6.2 Step 1 发现实际技术栈

检查

```text
Cargo.toml
pyproject.toml
requirements*.txt
package.json
pnpm-lock.yaml
apps/
crates/
workers/
docs/
schemas/
android/
src-tauri/
```

读取现有启动和测试脚本

### 6.3 Step 2 建立当前能力矩阵

至少覆盖

```text
Audio capture
Realtime transport
ASR
Translation
Explanation
Glossary
PPT/PDF/image
Timeline/event store
Replay/export
Android
DingTalk miniapp
DingTalk A1
Privacy/cloud policy
Testing
Packaging
```

### 6.4 Step 3 编写审计 ADR 草案

创建或更新

```text
docs/audits/INITIAL_REPO_AUDIT.md
docs/adr/ADR-001-a1-not-critical-path.md
docs/adr/ADR-002-rust-core-python-workers.md
docs/adr/ADR-003-local-sqlite-object-store.md
docs/adr/ADR-004-native-android-primary-capture.md
docs/adr/ADR-005-event-sourced-revisions.md
docs/adr/ADR-006-gpu-priority-scheduler.md
docs/adr/ADR-007-multimodal-assets-on-timeline.md
docs/adr/ADR-008-cloud-opt-in.md
```

若仓库已经有同类 ADR，不重复创建，改为建立映射和差异说明

### 6.5 Step 4 建立确定性 Mock 闭环

只有在审计完成后才开始实现

第一条垂直切片

```text
Mock audio source
  → event protocol
  → mock ASR partial and final
  → mock translation
  → mock explanation
  → SQLite event store
  → live UI
  → stop session
  → replay and export
```

该切片不能依赖外部 API、真实麦克风或 GPU

### 6.6 Step 5 建立基准框架

创建

```text
benchmarks/README.md
benchmarks/fixtures/manifest.yaml
benchmarks/asr/
benchmarks/translation/
benchmarks/multimodal/
docs/test-runs/
```

基准结果必须记录

- commit
- machine
- GPU driver
- CUDA
- model and revision
- quantization
- sample license
- command
- metrics
- failure

## 7 目标事件协议

先检查现有协议，缺失时实现以下基础包络

```json
{
  "event_id": "uuidv7",
  "schema_version": "1.0.0",
  "session_id": "session_id",
  "source_id": "source_id",
  "sequence": 1,
  "event_type": "segment.finalized",
  "captured_at_monotonic_ns": 0,
  "captured_at_wall": "2026-08-24T00:00:00-07:00",
  "ingested_at": "2026-08-24T00:00:00-07:00",
  "correlation_id": "correlation_id",
  "causation_id": null,
  "content_hash": "sha256:placeholder",
  "payload": {}
}
```

要求

- Schema 版本化
- Rust、Python、TypeScript 和 Kotlin 类型由单一 Schema 生成或通过契约测试保持一致
- 未知事件可以安全忽略或隔离
- 事件序号、重复、缺口和迟到行为有测试
- 二进制音频保存在对象文件，事件只保存引用和校验和

## 8 核心接口

至少建立以下 Provider 接口

```text
AudioSource
AsrProvider
TranslationProvider
ExplanationProvider
VisionProvider
EmbeddingProvider
TtsProvider
AssetParser
CloudEgressPolicy
```

所有 Provider 支持

- 能力发现
- 版本和模型 ID
- 健康检查
- 取消
- 超时
- 结构化错误
- 指标
- Mock 实现

## 9 Android 要求

Android 阶段必须实现

- Kotlin 原生工程
- ForegroundService
- AudioRecord
- 单声道 PCM
- Opus 编码
- 本地持久化滚动缓存
- WSS
- 来源序号
- ACK 和幂等重传
- 二维码配对
- 断网恢复
- 系统可见录音通知
- 暂停、停止和安全收尾
- 锁屏、切应用、Wi-Fi 切蜂窝和 90 分钟真机测试

在服务器 ACK 前不能删除本地块

## 10 DingTalk A1 要求

建立独立实验工具

```text
tools/a1-probe/
apps/dingtalk-miniapp/
```

不要让未验证 A1 能力污染核心接口

A1 已知接口适配目标

```text
startDingerRecord
getDingerDeviceStatus
query audio file list
query file by minutes id
query AI summary
summary completed event
```

使用 AIALRA `session_id` 生成 `businessOrder`

保存原始响应前执行脱敏

对实时能力的任何结论都必须附带

- DingTalk version
- A1 firmware
- account and permission category without secrets
- request and event type
- observed latency
- whether audio or transcript is incremental
- repeatability

若没有实时流，明确记录 `not publicly available or not observed`，继续 Android 主链路

## 11 ASR 基准要求

首轮候选

```text
faster-whisper large-v3-turbo
FunASR streaming candidate
SenseVoiceSmall or Fun-ASR-Nano candidate
NVIDIA streaming candidate if installable
```

至少输出

```text
Chinese CER
English WER
Mixed-language error analysis
Proper noun recall
First partial p50/p95
Finalization p50/p95
RTF
VRAM peak
CPU usage
90-minute stability
```

不能仅引用公开 benchmark 决定默认模型

## 12 翻译解释要求

翻译分为

```text
partial translation
final translation
translation revision
```

稳定翻译输入包含

- 当前稳定片段
- 最近上下文
- 术语表
- `do_not_translate`
- 当前页面标题或关键文本

解释代理只读取稳定片段

结构化输出至少包含

```text
summary
missing_context
rare_terms
possible_asr_errors
review_questions
evidence_segment_ids
asset_page_ids
confidence
```

所有引用必须验证存在

模型输出不能直接执行工具或改变系统策略

## 13 多模态要求

支持

```text
PPTX
PDF
DOCX
PNG
JPEG
WEBP
```

推荐

```text
python-pptx for PPT semantic extraction
LibreOffice headless for rendering
Docling for general document parsing
OCR only when text layer is unavailable
Qwen3-VL small model for selected pages
```

每个资产和页面必须有稳定 ID

中途上传材料的验收用例

1. 会话正在运行
2. 已经有稳定字幕
3. 用户上传 PPTX 或图片
4. 资产逐步解析
5. 下一次解释引用有效页面 ID
6. UI 显示缩略图和证据
7. 会后回放仍能恢复对应关系

## 14 GPU 调度要求

优先级固定为

```text
P0 ingest persistence control
P1 ASR
P2 final translation
P3 explanation
P4 vision and embedding
P5 post-session work
P6 TTS experiments
```

实现

- 模型档案
- 实测显存
- 队列长度
- 取消
- 超时
- OOM 捕获
- 自动降级
- ASR 健康保护

当 ASR 队列拥塞时先暂停视觉和自动解释

## 15 隐私要求

默认

```text
LOCAL_ONLY=true
RAW_AUDIO_RETENTION_DAYS configurable
CLOUD_TEXT_ALLOWED=false
CLOUD_MULTIMODAL_ALLOWED=false
CAMERA_CAPTURE=false
SCREEN_CAPTURE=false
```

会话创建必须记录录音许可确认

录音期间必须有明显状态

严禁隐蔽录音功能

任何云端调用写入审计事件

## 16 测试命令

根据实际仓库调整，但目标至少包含

Rust

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

Python

```bash
ruff check workers benchmarks tools
mypy workers benchmarks tools
pytest -q
```

Frontend

```bash
pnpm lint
pnpm typecheck
pnpm test
pnpm build
pnpm test:e2e
```

Android

```bash
./gradlew lint
./gradlew test
./gradlew assembleDebug
```

协议

```text
Schema compatibility tests
Rust ↔ Python contract tests
Rust ↔ TypeScript contract tests
Rust ↔ Kotlin contract tests
```

若某个工具尚未配置，不要伪造通过；先建立最小配置或在报告中写明缺失

## 17 文档要求

维护

```text
PROJECT_STATE.md
docs/CHANGELOG.md
docs/KNOWN_ISSUES.md
docs/DECISIONS.md or docs/adr/
docs/TEST_RUNS.md or docs/test-runs/
docs/BENCHMARKS.md
docs/MODEL_LICENSES.md
docs/PRIVACY_BOUNDARIES.md
```

技术文档遵守 AIALRA 人类可读写作规则

- 先说对象和结论
- 术语首次出现时立即解释
- 区分事实、推断和待验证
- 精确数字写来源、测量或计算方法
- 图表有图名或表名
- 长报告使用连续编号和引用
- 不把营销说法写成工程事实

若环境有 PowerShell，运行人类可读中文检查器

## 18 Git 提交规则

- 保持默认分支稳定
- 大功能使用 `feat/` 分支
- 每个垂直切片通过测试后提交
- Conventional Commits
- 不提交生成模型、私人数据和大基准音频
- 测试夹具必须有许可证说明
- 提交前运行 Secret 扫描

建议分支

```text
chore/initial-repo-audit
feat/event-core-mock-pipeline
bench/local-asr-candidates
feat/android-reliable-recorder
feat/bilingual-translation-glossary
feat/explanation-orchestrator
feat/multimodal-assets
feat/dingtalk-a1-adapter
```

## 19 当前首轮的完成定义

本次 Codex 任务至少完成

1. 只读审计
2. 审计报告
3. 研究包进入仓库文档目录
4. ADR 映射或草案
5. 当前能力矩阵
6. 可执行的近期任务计划
7. 基础构建和测试现状记录
8. 识别一个最小垂直切片
9. 在不破坏现有工作的前提下实现该切片，或在存在阻塞时完成可运行骨架
10. 真实运行和验证，不只写文档

若仓库已经远超该阶段，选择 Backlog 中第一个未通过门禁的任务

## 20 每轮完成报告

使用以下格式

```text
Summary
- 本轮完成了什么

Scope status
- 全局目标完成度
- 本轮任务完成度
- 当前门禁

Repo audit
- Branch
- Commit before
- Dirty files before
- Existing implementation reused

Files changed
- path: purpose

Commands and tests
- command: pass/fail/not run

Real validation
- page or app flow
- device
- model
- fixture
- result

Performance
- p50/p95 if measured
- VRAM peak
- long-run duration

Security and privacy
- secrets touched
- private data used
- cloud egress

Known limitations
- fact
- impact
- next verification

Commit
- branch
- commit
- pushed yes/no

Next task
- exact Backlog ID
```

不要询问已经能够通过仓库审计自行回答的问题

遇到外部凭据、A1 真机、课程许可或设备不可用时，完成所有不依赖该资源的实现、Mock、测试和探针，再把唯一剩余阻塞写入报告
