# 模型运行库清单

## 1 当前运行组合

- ASR：配置文件设定 `faster-whisper small` 处理英语 4 秒稳定窗口，模型由 faster-whisper 首次加载到 Hugging Face 缓存
- 翻译和讲解：本机已有的 `qwen2.5:14b-instruct` 通过 Ollama 提供中文翻译和证据讲解
- 失败回退：项目源码中的确定性规则保留原文和证据关系

## 2 运行库

- `faster-whisper` 和 CTranslate2 负责 Whisper 推理
- Windows GPU 运行使用项目 Python 环境中的 NVIDIA cuBLAS、cuDNN 和 NVRTC wheel
- Ollama 由本机独立服务提供，模型权重不进入本仓库

## 3 发布门禁

模型权重、运行库和数据集许可证需要在分发安装包前重新核验

当前版本只完成本机开发验证，没有重新分发模型权重

模型清单改变时，必须记录模型版本、来源、许可证、量化、语言、显存和基准结果

DeepSeek、Qwen3-VL、Docling、FunASR 和其他候选仍是未来 Provider，不属于当前默认运行组合
