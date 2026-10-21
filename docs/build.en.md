# Build and Packaging

[中文](build.md) · **English**

## Common prerequisites

- Rust stable (installing it with `rustup` is enough)
- Node 18+ (builds the frontend and the Tauri CLI)
- The first packaging run downloads the platform's bundler tooling over the network (NSIS / WiX on Windows, linuxdeploy on Linux), so make sure the network is reachable

```bash
npm install
npx tauri dev      # dev mode: vite hot reload + desktop window
npx tauri build    # build every bundle from tauri.conf.json's bundle.targets
```

To run just the tests (no Tauri dependency tree needed):

```bash
cargo test --workspace --exclude desktop
```

## Windows

Requires the **MSVC toolchain** (Visual Studio Build Tools, with the "Desktop development with C++" workload checked) and the **WebView2 Runtime** (bundled with Windows 10 2004+ / Windows 11; if it is absent, the installer pulls it over the network).

```powershell
npx tauri build --bundles nsis    # target\release\bundle\nsis\LLM Lite Proxy_x.y.z_x64-setup.exe
npx tauri build --bundles msi     # target\release\bundle\msi\*.msi (downloads the WiX Toolset on first run)
```

The installer UI defaults to Simplified Chinese with an English fallback (`bundle.windows.nsis.languages` in `src-tauri/tauri.conf.json`).

The "Delete the application data" check box on the uninstall confirmation page is extended by `src-tauri/nsis-hooks.nsh`: when it is ticked, `~/.llm_lite_proxy` is removed along with the app-data directories Tauri clears on its own. **If you rename the configuration folder, update that file too**, otherwise uninstalling leaves an unmanaged directory behind.

## macOS

```bash
xcode-select --install
npx tauri build --bundles dmg,app
```

Artifacts land in `target/release/bundle/macos/*.app` and `target/release/bundle/dmg/*.dmg`.
Without code signing and notarization, the first launch has to be allowed under "System Settings → Privacy & Security", or the quarantine attribute removed with `xattr -dr com.apple.quarantine`.

## Linux

Debian / Ubuntu family dependencies:

```bash
sudo apt update
sudo apt install -y libwebkit2gtk-4.1-dev build-essential curl wget \
  file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev patchelf
```

```bash
npx tauri build --bundles deb,appimage
```

The `rpm` target needs `rpmbuild` present in the environment, and `appimage` needs network access to pull linuxdeploy.

## CI

`.github/workflows/build.yml` is a Windows / macOS / Linux three-platform matrix: it builds the installers on `push tag v*` or manual trigger and uploads them as Actions artifacts; on a tag they are also attached to the GitHub Release.

> This workflow ran once across the three-platform matrix on 2026-10-20: Windows produced `nsis` + `msi`, macOS produced an arm64 `dmg` on an Apple Silicon runner (`file` in the log reports `Mach-O 64-bit executable arm64`), and Ubuntu 22.04 produced `deb` + `AppImage`. Only the Windows artifact has actually been installed and run, though — the other two are "builds green, never launched on real hardware".

## Release checks

```bash
npm run build                      # tsc type check + vite output
cargo test --workspace --exclude desktop
cargo clippy --workspace --exclude desktop -- -D warnings
npx tauri build --bundles nsis     # or the platform you are on
```
