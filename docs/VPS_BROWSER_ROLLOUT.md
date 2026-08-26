# 浏览器与 VPS 上线方案

## 1 结论

当前最合适的主入口不是继续要求安装 Android 客户端，而是让电脑和手机都使用同一个 HTTPS 网页。浏览器负责取得麦克风许可和发送音频；VPS 负责鉴权、接收、持久化、ACK、事件时间线和故障恢复。

现有 Android 客户端仍有价值，但定位改为“长时录音与弱网增强入口”。DingTalk A1 保留为“更好收音质量的同步录制与会后补偿入口”，不再阻塞网页主链路。

## 2 为什么能直接使用浏览器

浏览器的 `getUserMedia()` 可以访问电脑、USB 麦克风和移动设备麦克风，但远程页面必须位于 HTTPS 安全环境，且每个站点都需要用户明确授权。[MDN](https://developer.mozilla.org/en-US/docs/Web/API/MediaDevices/getUserMedia)

Cloudflare 代理和 Cloudflare Tunnel 都支持 WebSocket；当前 VPS 已有稳定的 Nginx、Cloudflare 代理 DNS 和 Authentik 共享网关，因此本轮复用现有入口，不再并行引入另一套 Tunnel 控制面。[Cloudflare WebSocket 文档](https://developers.cloudflare.com/network/websockets/) [Cloudflare Tunnel FAQ](https://developers.cloudflare.com/cloudflare-one/faq/cloudflare-tunnels-faq/)

## 3 当前部署形态

```text
浏览器麦克风
    ↓ HTTPS / WSS
Cloudflare 代理 DNS
    ↓
VPS Nginx
    ↓ Authentik 登录与授权
Rust 核心：落盘、ACK、事件、停止
    ↓
Python Worker：CPU ASR、材料解析
    ↓
本地 Ollama：翻译、讲解
```

Authentik 官方提供 Proxy Provider 和 Forward Auth 两种接入方式。AIALRA VPS 已有共享鉴权网关，并已经具备统一身份头覆盖、伪造头拒绝和应用清单同步，因此继续通过该网关接入，不直接在新应用里保存 Authentik 客户端秘密。[Authentik Proxy Provider](https://docs.goauthentik.io/add-secure-apps/providers/proxy/) [创建 Proxy Provider](https://docs.goauthentik.io/add-secure-apps/providers/proxy/create-proxy-provider/)

## 4 操作顺序

### 4.1 发布前

1. 完成浏览器界面、核心服务和 Worker 的本地测试。
2. 生成只包含 Git 跟踪文件的候选版本。
3. 扫描录音、逐字稿、课件、密钥、真实内部地址和二进制覆盖缺口。
4. 只有发布门禁为 `pass` 才创建或更新远程仓库。

### 4.2 VPS

1. 把候选版本放入不可变版本目录。
2. 在版本目录构建 Core、Worker 和固定版本的 Ollama 容器。
3. 数据、模型和 Ollama 权重放在版本目录之外。
4. Core 只映射到 VPS 回环端口，公网不能绕过 Nginx。
5. 先验证回环健康，再拉取轻量本地模型。

### 4.3 域名与鉴权

1. 使用 VPS 上受限的 Cloudflare 凭据创建一条代理 A 记录，目标复用现有源站记录。
2. 先安装仅支持 ACME 与 HTTPS 跳转的 Nginx 配置。
3. 申请站点证书，再切换到完整 TLS 配置。
4. 在共享鉴权应用清单加入站点并同步 Authentik 回调。
5. 验证匿名访问和伪造身份头都只能得到登录跳转。

### 4.4 功能验收

1. 匿名打开首页，确认跳转 Authentik。
2. 登录后运行内置合成课程，确认 SSE 时间线、翻译和解释卡。
3. 使用合成音频或用户明确许可的短句，确认 WSS、音频 ACK 和安全停止。
4. 上传不含隐私的测试材料，确认页面提取和证据引用。
5. 检查容器重启后会话仍存在，且没有音频或逐字稿进入普通日志。

## 5 性能边界

VPS 当前没有 GPU。CPU `faster-whisper tiny + int8` 和 1.5B 本地模型用于展示，不足以提前承诺课程精度或长课堂实时延迟；有 GPU 的本地 Worker 继续使用更高质量模型。

生产形态建议增加 GPU 计算连接器：VPS 仍先持久化音频并返回 ACK；受信任的 GPU 主机通过主动出站连接领取已保存任务并回传追加事件。GPU 主机断线时录音继续安全保存，恢复后再补算。这个结构避免把家庭或办公电脑暴露到公网，也避免模型故障影响停止录音。

## 6 下一轮量化门禁

- 浏览器连续录音 30 分钟和 90 分钟；
- Chrome、Edge、Safari 的前台、后台和屏幕锁定行为；
- 音频块确认完整率与断网恢复；
- VPS CPU ASR 的 p50、p95 和队列增长；
- GPU 连接器启用后的同一音频对照；
- 英文课程、中英混说、生僻术语和噪声样本；
- Authentik 有权、无权、会话过期和注销；
- 代码回滚不修改用户数据目录。

## 7 已知取舍

- 浏览器刷新会丢失尚未收到 ACK 的内存音频；Android 已有持久缓存，网页 IndexedDB 缓存仍待实现。
- 浏览器后台与锁屏行为由系统限制，手机网页不能完全替代 Android 前台服务。
- VPS 小模型的翻译和讲解质量低于本地高性能 GPU 配置。
- A1 第三方连续音频仍无公开证据，只承担控制和会后补偿。
