# 构建与打包

**中文** · [English](build.en.md)

## 通用前置

- Rust stable（`rustup` 安装即可）
- Node 18+（构建前端与 Tauri CLI）
- 首次打包会联网下载对应平台的打包工具（Windows 的 NSIS / WiX，Linux 的 linuxdeploy），请保证网络可达

```bash
npm install
npx tauri dev      # 开发模式：vite 热更新 + 桌面窗口
npx tauri build    # 按 tauri.conf.json 的 bundle.targets 打全部包
```

只想跑测试（不需要 Tauri 依赖树）：

```bash
cargo test --workspace --exclude desktop
```

## Windows

需要 **MSVC 工具链**（Visual Studio Build Tools，勾选「使用 C++ 的桌面开发」）和 **WebView2 Runtime**（Windows 10 2004+ / Windows 11 自带；没有的话安装程序会联网拉取）。

```powershell
npx tauri build --bundles nsis    # target\release\bundle\nsis\LLM Lite Proxy_x.y.z_x64-setup.exe
npx tauri build --bundles msi     # target\release\bundle\msi\*.msi（首次会下载 WiX Toolset）
```

安装包界面默认简体中文，带英文回退（`src-tauri/tauri.conf.json` 的 `bundle.windows.nsis.languages`）。

卸载确认页的「删除应用程序数据」复选框由 `src-tauri/nsis-hooks.nsh` 扩展过：勾上时除了 Tauri 默认的 app-data 目录，`~/.llm_lite_proxy` 也会一并删除。**改了配置目录名就要同步这个文件**，否则卸载会留下一个没人管的空目录。

## macOS

```bash
xcode-select --install
npx tauri build --bundles dmg,app
```

产物在 `target/release/bundle/macos/*.app` 与 `target/release/bundle/dmg/*.dmg`。
未做代码签名与公证时，首次打开需要在「系统设置 → 隐私与安全性」里允许，或 `xattr -dr com.apple.quarantine` 去掉隔离属性。

## Linux

Debian / Ubuntu 系依赖：

```bash
sudo apt update
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget \
  file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

```bash
npx tauri build --bundles deb,appimage
```

`rpm` 目标需要环境里有 `rpmbuild`，`appimage` 需要联网拉 linuxdeploy。

## CI

`.github/workflows/build.yml` 是 Windows / macOS / Linux 三平台矩阵，`push tag v*` 或手动触发时构建安装包并上传为 Actions 产物；打 tag 时同时挂到 GitHub Release。

> 这份 workflow 已在 2026-10-20 跑通一次三平台矩阵：Windows 出 `nsis` + `msi`，macOS 在 Apple Silicon runner 上出 arm64 的 `dmg`（日志里 `file` 显示 `Mach-O 64-bit executable arm64`），Ubuntu 22.04 出 `deb` + `AppImage`。但**只有 Windows 的产物在本机真正装过、跑过**，另两个平台属于「构建通过、未实机运行」。

## 发布检查

```bash
npm run build                      # tsc 类型检查 + vite 产物
cargo test --workspace --exclude desktop
cargo clippy --workspace --exclude desktop -- -D warnings
npx tauri build --bundles nsis     # 或对应平台
```
