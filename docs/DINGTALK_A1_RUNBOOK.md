# DingTalk A1 真机验证手册

## 1 目标

本手册验证 A1 是否能可靠承担高质量同步录音、设备控制和会后补偿

它不会把 `streaming` 状态解释为第三方已经取得音频流

连续 PCM 或增量逐字稿只有在官方合同或真机数据证据出现后才能标记为通过

## 2 已接入接口

- 控制录音：`POST /v1.0/dvi/devices/recording/control` 启动和停止 A1，并绑定 AIALRA 会话
- 查询音频列表：`POST /v1.0/dvi/device/audio/list` 在会后发现设备录音
- 获取下载信息：`POST /v1.0/dvi/device/audio/download` 下载 A1 原始音频用于对账
- 查询听记：`GET /v1.0/dvi/audios/minutes` 获取听记和转录资源
- 创建离线 ASR：`POST /v1.0/dvi/asr/transcriptions` 对指定音频发起官方离线转写
- 查询离线 ASR：`GET /v1.0/dvi/asr/transcriptions` 读取任务结果
- 查询智能总结：`POST /v1.0/minutes/smartdevice/aisummary` 获取 A1 官方分析作为会后补偿

## 3 准备条件

- DingTalk 企业内部应用已经获得相应 DVI 权限
- 测试操作者的 `userId`、团队 `teamCode` 和短期访问令牌可用
- A1 已绑定到测试组织、固件更新完成且电量和存储充足
- 测试课程已经获得录音许可
- 电脑和 Android 手机处于可控网络，时间已经同步

## 4 服务器探针

- 第一步，通过环境变量提供 `DINGTALK_ACCESS_TOKEN`、`DINGTALK_TEAM_CODE` 和 `DINGTALK_USER_ID`

不要把值写进 `.env.example`、终端历史、截图或问题单

- 第二步，先使用 `uv run python -m tools.a1_probe` 的默认 dry-run 查看方法、路径和请求体

只有请求字段和会话 ID 正确时才增加 `--execute`

- 第三步，启动正式 AIALRA 会话

核心服务会把 AIALRA `session_id` 放入 `outBizData.businessOrder`，并记录 `dingtalk.recording.started` 事件

- 第四步，在钉钉小程序中调用 `readA1Status`，记录设备 ID 的脱敏值、固件、录音状态、电量和存储

状态结果只用于设备诊断

- 第五步，课程结束时先发送 A1 停止命令，再停止本地会话

保留本地音频块、A1 录音 ID、听记 ID 和官方完成事件之间的映射

## 5 小程序前台音频帧探针

- 第一步，在钉钉开发者工具中载入 `apps/dingtalk-miniapp` 的逻辑，并配置局域网 WebSocket 安全域名

- 第二步，把核心服务显式绑定到局域网地址

完成验证后恢复 `127.0.0.1`，避免长期暴露无认证的开发接口

- 第三步，让小程序保持前台，运行 `ForegroundProbe`

核对每个发送序号都有 `audio.ack`，重复帧返回 `duplicate=true`

- 第四步，把小程序切到后台并记录停止时刻

记录于本次验证计划的目标时长为 90 分钟，预期结果是前台录音能力无法满足锁屏主链路，这个失败属于已知合同边界

## 6 会后对账

- A1 控制成功率：每次正式测试的启动和停止均有请求 ID 或明确失败原因
- 录音发现：停止后能通过设备或听记 ID 找到唯一录音
- 音频完整性：记录于本次验证计划的 A1 与本地时间轴有效时长差不超过 1%
- 文本覆盖：A1 官方转录和本地稳定字幕均能映射到同一课程时段
- 术语差异：报告双方关键术语召回，同时保留总字错率
- 补偿恢复：本地缺失时能导入 A1 音频，旧事件继续保留
- 凭据安全：日志、SQLite、对象目录和导出中没有访问令牌

## 7 判定

满足控制、录音发现、音频下载和听记对账时，A1 会后补偿链路可以标记为通过

只有取得连续音频字节或增量逐字稿事件，并能记录授权范围、时序、延迟、断线和重连行为时，A1 实时链路才能标记为通过

设备状态包含 `streaming` 或产品页面宣称实时转写都不满足这个证据要求 [1], [2], [3], [4]

## 8 参考资料

[1] DingTalk, “startDingerRecord,” 2026. [Online]. Available: https://open.dingtalk.com/document/development/jsapi-start-dinger-record

[2] DingTalk, “getDingerDeviceStatus,” 2026. [Online]. Available: https://open.dingtalk.com/document/development/jsapi-get-dinger-device-status

[3] DingTalk, “RecorderManager.onFrameRecorded,” 2026. [Online]. Available: https://open.dingtalk.com/document/development/jsapi-recorder-manager-on-frame-recorded

[4] Alibaba Cloud, “DingTalk OpenAPI SDK for Go,” 2026. [Online]. Available: https://github.com/alibabacloud-go/dingtalk
