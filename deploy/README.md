# 私有 GPU 混合部署

这套部署把浏览器音频经 HTTPS/WSS 发送到受 Authentik 保护的服务器，Rust Core 先持久化并 ACK，再由 Windows RTX GPU Agent 主动领取模型任务

VPS 不运行 ASR 或小型假替代模型，本机 Agent 离线时服务器只安全保存和排队

## 1 运行关系

```mermaid
flowchart TD
  Browser[电脑或手机浏览器] -->|HTTPS + WSS| Edge[Cloudflare 与 Nginx]
  Edge --> Auth[Authentik]
  Auth --> Core[Rust Core]
  Core --> Data[(SQLite WAL 与对象目录)]
  Core --> Queue[持久模型任务]
  Core --> Notes[ReadWeave 私有 ETAPI]
  Agent[Windows RTX GPU Agent] -->|私有出站领取| Gateway[私有 Worker Gateway]
  Gateway --> Queue
  Agent --> ASR[faster-whisper small CUDA float16]
  Agent --> LLM[Ollama 7B 段落翻译与滚动讲解]
  Agent --> Summary[Ollama 14B 最终总结]
  Agent --> VLM[Qwen3-VL 8B 课件理解]
```

## 2 服务器准备

1. 从 `.env.example` 生成只存在于服务器的环境文件
2. 设置真实站点主机名、持久数据目录、Worker 令牌 SHA-256 摘要和明确的历史会话所有者
3. 为 Worker Gateway 准备私有网络接口与最小访问策略
4. 构建并启动 Core，确认健康接口和持久目录
5. 把站点加入共享 Authentik 入口，确认匿名请求和伪造身份头都不能绕过登录
6. 安装 Nginx 配置，确认公开 `/internal/` 固定拒绝
7. 使用 `nginx/readweave.conf.template` 保护 ReadWeave 页面，并让公网 `/etapi/` 固定返回 `404`
8. 把 Core 接入 ReadWeave 私有容器网络或回环端口，确认浏览器无法取得 ETAPI Token
9. 使用合成课程执行多设备租约、离线恢复、Core 重启和真实 Provider 门禁

部署器依赖现有共享 Authentik 与 Nginx 控制面，相关根目录通过 `AIALRA_PLATFORM_ROOT` 显式传入，仓库不保存真实生产目录

## 3 Windows GPU Agent

默认 ASR 为 `faster-whisper small + CUDA float16`，7B 模型处理连贯段落翻译和滚动讲解，14B 模型处理停止后的最终总结，Qwen3-VL 8B 在实时队列排空后处理图片：

```powershell
ollama pull qwen2.5:7b-instruct # Coherent paragraph translation and rolling explanations
ollama pull qwen2.5:14b-instruct # Final course summaries only
ollama pull qwen3-vl:8b-instruct
```

本机守护脚本会验证模型、主动启动 Ollama、完成 ASR 与 7B 预热，再让 GPU Agent 领取生产任务

ASR 拥有最高优先级，7B 翻译和滚动讲解只接收服务端整理后的连贯段落，14B 最终总结和视觉模型保持低优先级单并发并按需卸载，避免多个大模型无约束常驻 16 GB 显存

先初始化一个随机令牌，服务器只保存它的 SHA-256 摘要：

```powershell
./scripts/initialize-gpu-agent-secret.ps1
```

再安装 Agent：

```powershell
./scripts/install-gpu-agent.ps1 -GatewayUrl "http://worker-gateway.example.invalid"
```

安装器会先确认系统 Ollama 中存在指定模型，再持久保存已验证的回环地址和模型名

安装器优先使用当前用户的任务计划程序，权限不足时退回用户登录启动项

状态与卸载：

```powershell
./scripts/get-gpu-agent-status.ps1
./scripts/uninstall-gpu-agent.ps1
```

## 4 必须保留的边界

- 环境文件、Cloudflare 凭据、DingTalk 凭据、Authentik 管理信息和 Worker 明文令牌不得进入 Git
- 数据目录独立于版本目录，代码回滚不能覆盖录音或数据库
- Core 只映射服务器回环端口，公开站点由 Nginx 与 Authentik保护
- Worker Gateway 只绑定私有接口，公开 Nginx 对 `/internal/` 返回拒绝
- Tailscale 或等价私有网络策略只允许指定 GPU 节点访问 Gateway
- 真实录音前必须记录许可确认，录音期间持续显示红色状态与停止入口
- Nginx 覆盖客户端身份头，只把 Authentik 返回的稳定用户标识传给 Core
- 同一项目只允许一个有效录音租约，租约密钥不进入 URL 和普通日志
- ReadWeave ETAPI 只允许 Core 私有访问，浏览器不能取得 ETAPI Token
- Provider 不可用时任务保持可重试，不返回 identity、deterministic 或 mock 结果
- 普通日志不记录音频、字幕、课件、令牌、私有地址或临时下载链接

## 5 上线门禁

- 身份：不同 Authentik 用户不能读取彼此项目，访问项目列表不能认领历史会话
- 多设备：同一用户看到相同项目和时间线，第二个录音设备得到 `409`
- GPU 离线：音频 ACK 完整，字幕为 0，任务进入持久队列
- GPU 恢复：任务自动完成，最终事件不重复
- Core 重启：任务与 `processing` 状态保留
- Provider：页面、事件与 Agent 返回的模型和设备一致
- 页面：同一页面完成许可、采集、停止、等待、字幕、译文、材料与讲解
- ReadWeave：离线时不阻塞模型链路，恢复后幂等补写且不覆盖人工区域
- 性能：先通过 30 分钟，再执行 90 分钟真实收音

ReadWeave 的目录、投影范围、冲突处理和恢复方法见 [ReadWeave 接入说明](../docs/READWEAVE_INTEGRATION.md)

## 6 回滚

部署前备份当前 Nginx 文件、鉴权应用清单和 Compose 环境文件

回滚只切换应用版本和代理配置，不删除持久数据目录，也不清理共享容器、证书或其他项目资源
