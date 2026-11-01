# RustMusic macOS 版（0.6.0，Apple Silicon）

上游项目：[LingyunStudio/RustMusic](https://github.com/LingyunStudio/RustMusic)（Tauri 2 + React + Rust，原本只有 Windows / Linux 发行版）。
本目录是基于上游 `c45d395`（v0.6.0）打的 macOS 适配构建，已在本机（macOS 26 / Apple Silicon / arm64）实测可运行。

## 交付物

| 文件 | 说明 |
| --- | --- |
| `RustMusic_0.6.0_aarch64.dmg` | 安装镜像，双击后把 RustMusic 拖进 Applications |
| `RustMusic.app` | 已签好（ad-hoc）的应用本体，可直接拖进 /Applications |
| `rustmusic-macos-port.patch` | 相对上游的全部改动（git diff，含新增文件） |

本机已安装到 `/Applications/RustMusic.app` 并启动验证。

## 安装 / 首次运行

1. 打开 `RustMusic_0.6.0_aarch64.dmg`，把 RustMusic 拖到 Applications。
2. 首次启动若提示「无法打开，因为无法验证开发者」：右键点图标 → 打开 → 再点「打开」；或
   `xattr -dr com.apple.quarantine /Applications/RustMusic.app`。
   （DMG 是 ad-hoc 签名、未走苹果公证，这是分发方式决定的，不是包坏了。）
3. 首次切到「设置 → 桌面歌词」等需要钥匙串的功能时，系统会问一次「RustMusic 想要使用钥匙串」——允许即可，凭据用系统钥匙串保存（与 Windows 版用凭据管理器等价）。

## macOS 适配做了什么

### 1. 窗口与外观：改用系统原生窗口
- 新增 `src-tauri/tauri.macos.conf.json`（Tauri 的平台配置合并文件）：macOS 上 `decorations: true` + `titleBarStyle: "Overlay"` + `hiddenTitle: true` + `trafficLightPosition: {x:16, y:18}` + `shadow: true`，Windows/Linux 继续走无框自绘窗口。
- `src/components/Titlebar.tsx`：macOS 不渲染自绘的最小化/最大化/关闭三按钮（否则会和红绿灯重复一套）。
- `src/components/Sidebar.tsx`：macOS 下侧栏顶部让出 34px，品牌区不再压在红绿灯下面。
- `src/main.tsx` + `src/utils.ts`：平台识别补上 `macos`（原本只区分 windows/linux），新增 `isMacOS()`。

### 2. 系统菜单栏（关键修复）
macOS 下如果应用没有菜单栏，WebView 里 **⌘C/⌘V/⌘A/⌘Z 全部失效**，⌘Q/⌘W/⌘H 也无处派发。
`src-tauri/src/main.rs` 在 `setup()` 里按 AppKit 约定手工拼了一套中文菜单（关于/服务/隐藏/退出、文件、编辑、显示、窗口），编辑项全部用系统 predefined 项，行为交还给 AppKit。

### 3. 交互与系统集成
- **关闭窗口 = 隐藏到菜单栏**：沿用上游的 `close_action` 设置，文案在 macOS 显示为「最小化到菜单栏」。
- **点 Dock 图标恢复窗口**：`main.rs` 里接管 `RunEvent::Reopen`——窗口隐藏后点 Dock 图标，系统只发 Reopen 事件，不接管就是「点图标毫无反应」。
- **菜单栏图标**：`src-tauri/icons/tray-template.png`（44×44 模板图，随亮/暗外观自动反相），`tray.rs` 用 `icon_as_template` 挂载；彩色 App 图标直接塞菜单栏会糊成一团脏色。
- **系统媒体控制**：沿用上游的 souvlaki（macOS 走 MPRemoteCommandCenter），控制中心/键盘媒体键可播放、暂停、上下曲，正在播放信息正常显示。
- **桌面歌词浮窗**：启用 tauri `macos-private-api` + `transparent(true)` + `visible_on_all_workspaces(true)`，实现真正的透明悬浮文字并跟随所有 Space；`src/desktop-lyrics/main.tsx` 里补了 macOS 分支（原逻辑把 macOS 误判成 Linux，会退化成实色黑底）。

### 4. 音频与凭据
- **钥匙串**：`Cargo.toml` 加 `keyring` 的 `apple-native` 特性——没有它 keyring 在 macOS 上根本编译不出后端。
- **WAV/OGG 解码修复（上游 bug，全平台受益）**：rodio 特性原本写了 `symphonia-wav`/`symphonia-vorbis` 却漏了 `symphonia-pcm`/`symphonia-ogg`，导致 `.wav`/`.ogg` 在资料库里能被扫到、一播放就报「无法解码该音频文件」。改成 meta 特性 `wav`/`vorbis` 后正常。

### 5. 界面文案
- 「在资源管理器中显示」→ macOS 显示「在访达中显示」。
- 更新说明按平台改口径：macOS 版不内置安装器，点「立即更新」跳 GitHub 发布页下载 dmg。
- 关于页脚：界面支持 Windows / macOS / Linux。

### 6. 打包
- `tauri.conf.json`：`bundle.targets` 在 macOS 配置里设为 `["app", "dmg"]`，`minimumSystemVersion: "11.0"`，`signingIdentity: "-"`（ad-hoc，arm64 上未签名二进制会被系统直接拒绝启动），DMG 窗口 660×400 布局。
- 图标补齐 32/128/128@2x/icns/ico。

## 本机验证结果

- 构建：`npm run tauri build` → RustMusic.app + RustMusic_0.6.0_aarch64.dmg（7.2 MB）✅
- 启动：/Applications 中启动正常，无框 Overlay 标题栏 + 中文菜单栏 ✅
- 音频：WAV（44.1k/16bit）与 m4a 均能播放，位置推进正常，CoreAudio 默认设备（跟随系统）✅
- 系统媒体控制：控制中心显示当前曲目，控制中心的播放键能驱动 App 播放 ✅
- 桌面歌词：透明浮窗正常显示并叠在其它窗口之上 ✅
- 关闭 → 隐藏到菜单栏（进程存活）；`open`/Dock 重新唤起窗口 ✅
- 库：从空库启动、添加文件夹、扫描、播放 ✅

## 已知限制

- **未公证**：ad-hoc 签名，别的 Mac 上首次打开需要右键「打开」放行（或清 quarantine 属性）。
- **钥匙串提示**：用 ad-hoc 签名时每个新构建的二进制指纹都不同，重新构建后首次读旧凭据会弹一次授权框；正式分发（固定签名身份）后只会出现一次。
- **菜单栏图标可能被刘海挡住**：菜单栏项目多时，新图标会被排到最左侧、落进 MacBook 的刘海区域而不可见（这是 macOS 的排布策略，不是程序问题）；此时用 Dock 图标或 ⌘Q 即可。
- **自动更新**：macOS 版走 GitHub 发布页手动下载 dmg，不做应用内安装。
- 仅 Apple Silicon（aarch64）构建；Intel Mac 需要另编 `x86_64-apple-darwin`。

## 重新构建

```bash
cd RustMusic
export RUSTUP_HOME=$PWD/../.rustup CARGO_HOME=$PWD/../.cargo
export PATH="$CARGO_HOME/bin:$PATH"
npm install --cache ../.npm-cache --no-audit --no-fund
npm run tauri build     # 产物在 src-tauri/target/release/bundle/{macos,dmg}
```