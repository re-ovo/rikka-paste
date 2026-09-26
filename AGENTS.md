# AGENTS.md

## 项目概述

Rikka Paste：Tauri 2 托盘常驻应用（macOS / Windows），在局域网内同步剪贴板的文本和图片。前端 React 19 + Tailwind 4 + Vite，包管理用 bun；核心逻辑全部在 `src-tauri/`（Rust）。代码注释、UI 文案、错误信息都用中文。

## 常用命令

```sh
bun install
bun tauri dev                    # 开发运行（会先启动 vite，端口 1420 固定）
bun run build                    # tsc 类型检查 + vite 构建前端（前端没有单独的 lint/test）
bun tauri build                  # 打包

cargo test --manifest-path src-tauri/Cargo.toml                      # 全部 Rust 测试
cargo test --manifest-path src-tauri/Cargo.toml message_roundtrip    # 单个测试（按名称过滤）
```

`clipboard/macos.rs` 的测试只在 macOS 上编译运行，`clipboard/windows.rs` 只在 Windows 上编译。

发布：同步修改 `package.json`、`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json` 的版本号（以及 `Cargo.lock`），然后推送 `v*` tag，`.github/workflows/release.yml` 会构建 macOS universal 和 Windows 包并创建 GitHub Release。前端的更新提示依赖 GitHub latest release 的 `tag_name` 与 app 版本比较。

## 架构

### 数据流

`sync::Engine`（`src-tauri/src/sync.rs`）是整个应用的核心，在 `lib.rs` 的 `setup` 里启动并作为 Tauri state 管理：

1. **发现**：mDNS 注册 `_rikkapaste._tcp.local.`，TXT 记录携带 `id`、`name`、`fp`（配对码派生密钥的指纹）、`v`（协议版本）。另开线程接收 browse 事件维护 `peers`。
2. **发送**：独立线程每 300ms 轮询 `clipboard::change_count()`，变化时 `clipboard::read()`，把内容封装成消息、加密，对每个"协议版本一致且指纹一致"的 peer 各开一个 TCP 短连接发送。
3. **接收**：tokio TcpListener（随机端口，通过 mDNS 公布）读帧，在 `spawn_blocking` 中解密、校验时间戳（±5 分钟）、写入本地剪贴板。
4. **UI 同步**：每次状态变化 `emit("state-changed", Snapshot)` 推送完整快照；前端（`src/App.tsx`，单文件）只通过 `get_state` / `update_config` / `generate_key` / `copy_secret` / `open_release` / `get_autostart` / `set_autostart` 这几个命令和事件交互，不自己轮询。

### 需要跨文件理解的约束

- **防回环靠 hash**：`Inner.last_hash` 记录最近同步内容的 SHA-256。远端内容写入本地后 watcher 会再读到它，只有字节完全一致才不会被再次发出。因此剪贴板读回的数据必须与写入时字节一致（例如 Windows 的 `trim_png` 去掉 GlobalSize 填充）。改剪贴板读写逻辑时要保住这一点。
- **协议兼容**：线上格式为 `u32 BE 帧长 || nonce(12) || ChaCha20-Poly1305 密文`，明文为 `u32 BE header 长度 || header JSON || 原始正文`。改动消息格式必须递增 `PROTOCOL_VERSION`（版本不同的设备互不发送；未公布 `v` 的旧客户端视为 "1"）。修改 `crypto.rs` 的 `SALT` 或 Argon2 参数会改变密钥和指纹，同样导致与旧版本不互通。
- **敏感/临时标记**：`clipboard/mod.rs` 定义了跨平台抽象（`Content`、`Read`、`WriteMode`），平台实现分别遵循 nspasteboard.org 约定（macOS）和 Windows 剪贴板历史排除格式。带敏感标记的内容读取时返回 `Read::Sensitive`，不会同步；`copy_secret` 用 `Concealed` 写入配对码。剪贴板同时有文本和图片时只取文本。图片统一以 PNG 传输（macOS 的 TIFF、Windows 的 CF_DIB 在读取时转码，Windows 写入时同时写 PNG 和 CF_DIB）。
- **Rust ↔ TS 类型需手动同步**：`Config`、`Snapshot`、`PeerView`、`LogEntry` 用 `serde(rename_all = "camelCase")` 序列化，`src/App.tsx` 顶部有对应的 TS 类型。`Config` 带 `#[serde(default)]`，新增字段对已有 `config.json` 向后兼容。
- **新增 Tauri 命令**要加到 `lib.rs` 的 `generate_handler!`；前端需要的窗口 API 权限要加到 `src-tauri/capabilities/default.json`。
- **开机自启**：用 `tauri-plugin-autostart`（macOS 为 LaunchAgent），状态以系统登录项为准，不存进 `Config`。自启时带 `--autostart` 参数；窗口在两份 `tauri*.conf.json` 里都是 `visible: false`，`setup` 中没有该参数时才显示。
- **窗口/平台差异**：关闭窗口只是隐藏（同步在后台继续），macOS 使用 Accessory 激活策略不占 Dock。macOS 用 Overlay 标题栏，`tauri.windows.conf.json` 在 Windows 上关闭系统装饰，由前端 `TitleBar` 自绘。`src-tauri/Info.plist` 中的 `NSBonjourServices` 必须与 `SERVICE_TYPE` 保持一致，否则 macOS 本地网络权限下无法发现设备。
- `Cargo.toml` 对 argon2、chacha20、poly1305、sha2 等在 dev profile 下单独开了 `opt-level = 3`（64 MiB Argon2 派生和大图加解密在未优化构建下很慢），不要删掉。
