# 更新日志

本文件记录本项目比较有影响的变更，版本号遵循 [语义化版本](https://semver.org/lang/zh-CN/)。
English notes for each release are on the [Releases](https://github.com/bluebirds37/llm_lite_proxy/releases) page.

## 0.1.0 - 2026-10-21

第一个可安装版本。

### 代理内核

- 纯 Rust 后端（`axum` + `reqwest` + `tokio`），一个进程同时暴露三个入站端点：`/v1/chat/completions`、`/v1/responses`、`/v1/messages`
- 协议分两级描述：协议族（`openai` / `anthropic`，决定鉴权头与错误体）+ API 面（`chat` / `responses` / `messages`，决定路径与转换）；任意入站面 × 任意上游面都通
- 流式（SSE）逐块转换；工具调用三向转换（含非法工具名清洗与响应端还原）；思考块 `thinking` / `reasoning_content` 与签名双向映射
- 模型路由：精确别名、`provider/model` 前缀直指、glob 模式按序匹配、兜底上游
- 客户端鉴权（`Authorization: Bearer` 与 `x-api-key` 都认）；上游 Key 留空则转发客户端 Key
- 实时请求日志：状态码、协议路径、模型、tokens、总耗时、首包耗时、被丢弃的参数、上游原始响应

### 桌面端

- Tauri 2 配置窗口：卡片式上游配置、分组式代理设置、请求日志、客户端接入片段
- 浅色 / 深色主题，窗口标题栏跟着一起变
- 配置统一在 `~/.llm_lite_proxy/config.json`，旧位置（各平台 app-data 的 TOML）启动时自动迁移一次

### 打包

- Windows：NSIS 安装包（简体中文界面，带英文回退）+ MSI；per-user 安装，开始菜单有卸载项，卸载时可勾选一并删除配置目录
- macOS（Apple Silicon）：`.dmg`；Linux：`.deb` 与 `.AppImage`
- `.github/workflows/build.yml` 三平台矩阵，打 tag 时自动挂到 GitHub Release

### 已知边界

- 上游协议族只有 OpenAI 兼容与 Anthropic 兼容两类，没有 Bedrock / Vertex / Gemini 的原生签名
- Responses ⇄ Messages 经 Chat Completions 中转，两次映射之间的语义间隙不做保真转换
- 没有重试、负载均衡和多 Key 轮询；一个请求只打一个上游
- Key 以明文存在本机配置文件里
- 界面文案目前只有中文
- macOS / Linux 产物只验证过「CI 能构建」，没在实机装起来跑过；所有平台都未做代码签名与公证

---

## 0.1.0 - 2026-10-21 (English summary)

First installable release.

- Pure-Rust conversion proxy exposing `/v1/chat/completions`, `/v1/responses` and `/v1/messages` in one process; protocol family + API surface as two independent axes, so any inbound surface reaches any upstream surface
- Streaming converted chunk by chunk; three-way tool-call mapping; thinking blocks and signatures mapped both ways
- Model routing by exact alias, `provider/model` prefix, ordered glob patterns and a fallback upstream
- Tauri 2 configuration window: upstream cards, grouped proxy settings, live request log, copy-paste client snippets; light/dark theme that also repaints the window title bar
- Configuration lives in `~/.llm_lite_proxy/config.json`, migrated automatically from the old per-platform TOML location
- Windows NSIS installer (Simplified Chinese UI, English fallback) with an uninstall shortcut and an opt-in removal of the configuration folder; macOS `.dmg` on Apple Silicon; Linux `.deb` and `.AppImage`
- Limits: only OpenAI-compatible and Anthropic-compatible upstreams; no retries, load balancing or key rotation; keys stored in plaintext; UI text is Chinese-only; macOS/Linux artifacts are build-verified but never launched on real hardware; nothing is code-signed or notarized
