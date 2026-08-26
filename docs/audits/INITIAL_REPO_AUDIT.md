# 初始环境审计

## 1 初始状态

审计开始时，工作区只有经用户提供的研究 ZIP，没有源码，也不是 Git 仓库

ZIP SHA-256 为 `9b80e0ceee91defd8a94acfe43e2a60aab1455f5cef53d842fc2643916979637`

包内清单逐项复核通过

工作区随后初始化为 `main` 分支 Git 仓库

当前没有创建提交，也没有推送远端

原始 ZIP 和解压副本由 `.gitignore` 排除，经过校验的文档副本保存在 `docs/planning/2026-08-24-initial-research/`

## 2 本机事实

系统显示以下本机事实：

- 操作系统：Windows 11 Pro for Workstations，构建 26100
- 处理器：Intel Core i9-13900KF，24 核、32 逻辑处理器
- 显卡：NVIDIA RTX 4080，16,376 MiB，驱动 591.86
- 内存：约 64 GiB
- Rust：1.95.0
- Node.js：24.13
- pnpm：10.33.4
- Python：项目环境 CPython 3.13.3
- Android SDK：android-36 和 android-37 已安装
- 本地大模型：Ollama，`qwen2.5:14b-instruct` 和 `qwen2.5:0.5b-instruct`

用户最初描述为 i9-13900K；本机查询结果是 i9-13900KF

两者主要差别是 KF 没有集成显卡，这不会改变 RTX 4080 本地模型路线

## 3 已有资产

本机存在独立 BabbleDeck 仓库

它已经覆盖实时音频 WebSocket、SSE、IndexedDB 恢复、Soniox、事件持久化、导出、Tauri、Capacitor 和 LiveKit

该仓库没有发现明确许可证文件，因此本项目只复用设计经验和公开接口模式，没有复制其业务代码

## 4 风险处理

- 系统显示端口 8080 和 11434 已被占用，配置文件设定项目使用 8787 和 8790
- 全局 Python 与项目环境不同；项目由 `uv.lock` 固定并使用本地 `.venv`
- Windows 缺少系统级 CUDA Toolkit 动态库；项目使用本地 NVIDIA wheel，并把 DLL 路径限制在模型进程
- 没有 Android 设备连接；APK 构建通过，真机门禁保持开放
- 没有 DingTalk 凭据；A1 请求只完成 dry-run 和模拟传输测试
