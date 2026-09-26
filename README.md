# Rikka Paste

极简的局域网剪贴板同步工具（macOS / Windows），常驻托盘。

- 同一局域网内、配对码相同的设备通过 mDNS 自动发现，复制的文本和图片会同步到其他设备
- 传输使用 ChaCha20-Poly1305 加密，密钥由配对码经 Argon2 派生
- 直接读写系统剪贴板，因此天然兼容 Maccy、Windows 剪贴板历史（Win+V）等工具
- 遵循剪贴板管理器通用的标记约定：
  - 带敏感/临时标记的内容（1Password、KeePass 等写入的密码）不会被同步
  - 可选择让同步来的内容不进入剪贴板历史（macOS 写入 `org.nspasteboard.TransientType`，Windows 写入 `CanIncludeInClipboardHistory=0`）

支持纯文本（单条上限 4 MB）和图片（单张上限 32 MB，统一以 PNG 传输，可在设置中关闭图片同步）。剪贴板里同时有文本和图片时只同步文本。

## 开发

```sh
bun install
bun tauri dev
```

首次运行时，macOS 会请求"本地网络"权限，Windows 防火墙会询问是否允许网络访问，都需要允许。
