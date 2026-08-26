# 录音隐私边界

## 1 默认策略

课程音频、字幕、译文、材料和讲解默认保存在本机 `data` 目录

云端文本与多模态出口保持关闭

DingTalk 只在用户配置企业凭据并启动正式课程时接收 A1 控制请求

## 2 产品门禁

- 正式会话必须勾选“已获得课程录音许可”
- 网页、Android 前台服务和系统通知持续显示录音状态
- 用户可以停止录音；停止后只处理已经确认接收的模型任务
- Android 音频先落盘，收到服务端 ACK 后才删除
- 原始课程材料使用内容寻址存储，文件名只作为清洗后的显示信息
- 日志记录 ID、状态和错误类别，不记录逐字稿、图片正文或访问令牌
- 摄像头默认关闭，当前版本没有摄像头采集代码
- 分享和云端 Provider 仍未开放，因此不会自动分发课程内容

## 3 用户责任

录音许可取决于适用法律、学校、课程和教师要求

产品中的许可确认只记录用户操作，不能替代真实许可

用户在录音、上传、导出或分享前仍需核对所在地区和课程规则 [1]、[2]

## 4 参考资料

[1] California Legislative Information, “Penal Code Section 632,” 2026. [Online]. Available: https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=PEN&sectionNum=632

[2] University of Southern California, “Course Recording Policy,” 2026. [Online]. Available: https://policy.usc.edu/sca-recording/
