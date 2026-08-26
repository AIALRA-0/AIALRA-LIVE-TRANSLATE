# AIALRA-LIVE-TRANSLATE 初始研究包

生成日期：2026-08-24

适用对象：项目所有者、Codex、后续工程代理

## 1 文件说明

<div align="center">

表 1.1 交付文件说明

| 文件 | 用途 |
| --- | --- |
| `01_INITIAL_RESEARCH_REPORT.md` | 完整项目初始报告，覆盖可行性、市场、竞品、钉钉 A1、架构、模型、数据、测试、路线图、风险与验收 |
| `02_CODEX_BOOTSTRAP_PROMPT.md` | 可直接交给 Codex 的首轮执行指令，要求先审计本地仓库，再按门禁推进 |
| `03_IMPLEMENTATION_BACKLOG.md` | 按依赖排序的工程 Backlog，包含任务编号、优先级、输入、产物与验收标准 |
| `04_EXECUTION_MANIFEST.yaml` | 供代理读取的机器可读执行清单，固定架构边界、阶段门禁和禁止事项 |
| `05_SOURCE_INDEX.md` | 本次调查使用的内部材料和外部一手来源索引 |
| `06_VALIDATION_REPORT.md` | 结构、引用、YAML、敏感信息和压缩包校验结果 |
| `07_PACKAGE_MANIFEST.sha256` | 研究包内文档的 SHA-256 校验值，便于传给 Codex 后检查文件完整性 |

</div>

## 2 当前证据边界

本次对话无法直接访问用户电脑上的 `AIALRA-LIVE-TRANSLATE` 本地目录，因此本研究包不声称已经核验本地源码、依赖、分支、未提交修改或现有测试

Codex 的第一项工作必须是只读审计本地仓库，并把实际状态写入 `docs/audits/INITIAL_REPO_AUDIT.md`

公开资料已经证实钉钉提供 A1 录音控制、设备状态、录音文件查询、分析结果查询与总结完成事件；公开资料还没有证实 A1 向第三方持续输出实时 PCM 音频流或实时逐字稿事件，因此 A1 实时直连只能作为验证支线，不能成为 MVP 唯一输入链路

## 3 建议阅读顺序

- 第一步，阅读 `01_INITIAL_RESEARCH_REPORT.md`，确认产品定位和架构边界

- 第二步，把 `02_CODEX_BOOTSTRAP_PROMPT.md` 作为 Codex 新任务的完整提示词

- 第三步，让 Codex 读取 `03_IMPLEMENTATION_BACKLOG.md` 和 `04_EXECUTION_MANIFEST.yaml`，只执行当前门禁允许的任务

- 第四步，Codex 完成仓库审计后更新 `PROJECT_STATE.md`、ADR 和测试记录，再进入实现

## 4 核心决定

- 产品定位：本地优先的多模态实时学习副驾
- 主录音链路：Android 原生长时录音或桌面音频采集
- 钉钉小程序链路：前台原型、设备控制和短时验证
- A1 链路：录音控制、会后文件导入、官方结果对账和能力探针
- 核心服务：Rust 模块化单体
- 模型服务：Python 独立 Worker，通过版本化协议连接 Rust
- 用户界面：Tauri 桌面端加 React 前端；Android 使用 Kotlin 原生采集端
- 本地存储：SQLite WAL 加文件对象目录
- 实时策略：字幕优先级最高，周期讲解和视觉分析只能使用剩余 GPU 资源
- 云端模型：默认关闭，用户显式开启后才发送最小化文本或选定图片
- BabbleDeck 关系：复用协议、事件模型、Web UI 与生产测试经验；避免复制现有普通会议平台
