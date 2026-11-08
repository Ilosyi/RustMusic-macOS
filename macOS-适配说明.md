# RustMusic macOS 版（0.7.1，Apple Silicon）

上游项目：[LingyunStudio/RustMusic](https://github.com/LingyunStudio/RustMusic)（Tauri 2 + React + Rust，官方只发 Windows / Linux）。
本仓库是在上游 **v0.7.1**（`3a7a452`）之上打的 macOS 适配分支，已在本机（macOS 26 / Apple Silicon / arm64）实测通过。

## 交付物

| 文件 | 说明 |
| --- | --- |
| `RustMusic_0.7.1_aarch64.dmg` | 安装镜像，双击后把 RustMusic 拖进 Applications（7.2 MB） |
| `RustMusic.app` | 已 ad-hoc 签名的应用本体，可直接拖进 /Applications |
| `rustmusic-macos-port.patch` | 相对上游 v0.7.1 的全部改动（`git diff`，含新增文件） |

SHA-256（dmg）：`09f4a35256b20400aadacb13b7857bb060929adb2efe3ad1ac8e6f2e24f0e381`

## 安装 / 首次运行

1. 打开 `RustMusic_0.7.1_aarch64.dmg`，把 RustMusic 拖到 Applications。
2. 首次启动若提示「无法打开，因为无法验证开发者」：右键点图标 → 打开 → 再点「打开」；或执行
   `xattr -dr com.apple.quarantine /Applications/RustMusic.app`。
   （构建是 ad-hoc 签名、未走苹果公证，这是分发方式决定的，不是包坏了。）
3. 首次使用会正常启动，**不会**弹钥匙串密码框（见下方「凭据存储」）。

## 0.7.1 相比 0.6.0 的上游变化（本版已一并适配）

- 三大界面体系：标准 / 拟物 / 液态玻璃，组件、视图按主题各有一份实现。
- 独立极简小窗（迷你模式）：360×118 无边框小窗，可拖拽、双击或 ⤢ 还原主窗口。
- 三大音源 SVIP 音质档位、真实音质显示。
- 上游修复：迷你模式长歌词跑马灯、资料库/我喜欢工具栏排版、WAV 解码（`symphonia-pcm`）。

## macOS 适配做了什么

### 1. 窗口与外观：改用系统原生窗口
- 新增 `src-tauri/tauri.macos.conf.json`（Tauri 平台配置合并文件，RFC 7396 数组整体替换）：macOS 上
  `decorations: true` + `titleBarStyle: "Overlay"` + `hiddenTitle: true` + `trafficLightPosition: {x:16, y:18}` + `shadow: true`，
  Windows / Linux 继续走无框自绘窗口。
- 三套标题栏（`Titlebar.standard|skeuo|liquid.tsx`）的按钮组统一加 `win-btns` 类，`src/index.css` 里
  `html[data-platform="macos"] .win-btns { display: none; }` 隐藏自绘按钮，避免与红绿灯重复一套。
- 三套侧栏（`Sidebar.standard|skeuo|liquid.tsx`）加 `app-sidebar` / `app-sidebar-card` 类，macOS 下
  顶部让出 34px（液态玻璃卡片额外下移），品牌区不再压在红绿灯下面。
- `src/main.tsx` + `src/utils.ts`：平台识别补上 `macos`（原本只区分 windows / linux），新增 `isMacOS()`。

### 2. 系统菜单栏（关键修复）
macOS 下应用没有菜单栏时，WebView 里 **⌘C / ⌘V / ⌘A / ⌘Z 全部失效**，⌘Q / ⌘W / ⌘H 也无处派发。
`src-tauri/src/main.rs` 在 `setup()` 里按 AppKit 约定手工拼了一套中文菜单（RustMusic / 文件 / 编辑 / 显示 / 窗口），
编辑项全部用系统 predefined 项，实际行为交还 AppKit。菜单栏显示为「RustMusic 文件 编辑 显示 窗口」。

### 3. 交互与系统集成
- **关闭窗口 = 隐藏到菜单栏**：沿用上游 `close_action` 设置，文案在 macOS 显示为「最小化到菜单栏」。
- **点 Dock 图标恢复窗口**：`main.rs` 接管 `RunEvent::Reopen`——窗口隐藏后点 Dock 图标系统只发 Reopen 事件，
  不接管就是「点图标毫无反应」。
- **菜单栏图标**：`src-tauri/icons/tray-template.png`（44×44 模板图，随亮/暗外观自动反相），`tray.rs` 用
  `icon_as_template` 挂载；彩色 App 图标塞进菜单栏会糊成一团脏色。
- **系统媒体控制**：沿用上游 souvlaki（macOS 走 `MPRemoteCommandCenter`），控制中心「正在播放」与键盘媒体键
  可播放 / 暂停 / 上下曲，封面与曲目信息正常。
- **桌面歌词浮窗**：启用 tauri `macos-private-api` + `transparent(true)` + `background_color(0,0,0,0)` +
  `visible_on_all_workspaces(true)`，实现真正的透明悬浮文字；`src/desktop-lyrics/main.tsx` 补 macOS 分支
  （原逻辑把 macOS 误判成 Linux，会退化成实色黑底）。
- **迷你小窗**：继承同一套无边框 + 透明能力，红绿灯与拖拽区域正常。

### 4. 音频与凭据（本版有两处与 0.6.0 不同）

**WAV / OGG 解码**：上游 0.7.1 已补 `symphonia-pcm`（WAV 可播）；本分支再补 `symphonia-ogg`，
OGG(Vorbis) 容器也能正常解码播放（`AUDIO_EXTS` 里一直列着 wav / ogg）。

**凭据存储改为本机文件（重要）**：
- 0.6.0 用 `keyring` + `apple-native`（系统钥匙串）。实测问题：Security.framework 把钥匙串条目的 ACL
  绑定到写入者二进制的 cdhash，而 ad-hoc 签名的哈希每次重新构建/升级都会变——应用读不回自己写的条目，
  启动时直接卡在系统密码弹窗后面（「RustMusic 想要使用你储存在钥匙串的“RustMusic”中的机密信息」），
  用户在弹窗里输密码才能进主界面。
- 因此 macOS 版改用 `src-tauri/src/secret_store.rs`：把凭据写进 `<应用数据目录>/credentials.json`
  （原子写入、`0600` 权限、仅当前用户可读），`credentials.rs` / `navidrome.rs` 的读写全部走这一层；
  Windows / Linux 仍用系统凭据管理器 / Secret Service，行为不变。
- 代价：文件是明文（但对其他用户不可读），且 0.6.0 时代存进钥匙串的旧条目不再读取——需要重新登录
  网易云 / 哔哩哔哩 / Navidrome 一次。换来的是**每次升级都不会再问钥匙串密码**。

### 5. 界面文案
- 「在资源管理器中显示」「在文件管理器中打开」→ macOS 显示「在访达中显示」。
- 关闭窗口项：「最小化到菜单栏」+「隐藏后音乐继续播放：点 Dock 图标或菜单栏图标可重新打开主界面，⌘Q 完全退出。」
- 更新说明按平台改口径：macOS 版不内置安装器，点「立即更新」跳 GitHub 发布页下载 dmg 手动安装。
- Navidrome 密码说明：macOS 显示「密码仅保存在本机应用数据目录（仅当前用户可读）」，不再提凭据管理器。
- 桌面歌词锁定提示：macOS 不提「托盘菜单」，只提主窗口按 L 解锁。
- 关于页脚：界面支持 Windows / macOS / Linux（WASAPI 独占输出仅 Windows）。

### 6. 打包
- macOS 平台配置里 `bundle.targets: ["app", "dmg"]`（基础配置是 `[]`，不覆盖就只产出裸二进制）、
  `minimumSystemVersion: "11.0"`、`signingIdentity: "-"`（ad-hoc；arm64 上未签名二进制会被系统直接拒绝启动），
  DMG 窗口 660×400 布局。`bundle.icon` 补齐 32 / 128 / 128@2x / icns / ico。
- `app.macOSPrivateApi: true`（透明窗口需要）。
- 注意：`bundle.macOS.category` 不是 tauri-utils 2.9.3 的合法字段，写进去构建会直接报 unknown field。

## 本机验证结果（macOS 26 / Apple Silicon）

- 构建：`npm run tauri build` → `RustMusic.app` + `RustMusic_0.7.1_aarch64.dmg`（7.2 MB）✅
- 启动：/Applications 中启动正常，无钥匙串弹窗，资料库空态干净 ✅
- 外壳：原生红绿灯 + 中文菜单栏「RustMusic 文件 编辑 显示 窗口」，三套主题（标准 / 拟物 / 液态玻璃）下
  侧栏品牌区都不压红绿灯 ✅
- 音频：m4a、WAV、OGG(Vorbis) 在 CoreAudio 默认设备正常播放，位置推进、暂停、切歌正常 ✅
  （OGG 用 ffmpeg 生成的 `Example.ogg` 实测：资料库识别为 `ogg`，播放位置推进到 0:04 / 0:06）
- 桌面歌词：「词」按钮打开 800×150 透明浮窗，文字浮在播放条之上、无底色 ✅
- 迷你小窗：360×118 小窗打开正常，⤢ 还原主窗口正常 ✅
- 系统媒体控制：控制中心「正在播放」显示曲目 / 应用图标，可播放暂停 ✅
- 关闭窗口 → 隐藏（进程存活）；点 Dock 图标 / `open -a` → `RunEvent::Reopen` 恢复窗口 ✅
- 设置页：最小化到菜单栏、macOS 更新说明、在访达中显示、输出设备「跟随系统默认（MacBook Air 扬声器）」✅

## 已知限制

- **无苹果公证**：首次打开必须右键 → 打开（或 `xattr` 清 quarantine），这是 ad-hoc 签名的必然结果。
- **菜单栏图标可能被刘海遮住**：刘海屏 MacBook 上菜单栏右侧被摄像头区切断，macOS 会把新加的状态项
  排到刘海下方（图标仍在，但看不见、点不到）。临时办法：关掉几个其他菜单栏图标，或直接用 Dock 图标
  （已实现 Reopen，隐藏后点 Dock 即可恢复）。功能本身没问题。
- **自动更新需手动安装**：点「立即更新」只跳 GitHub 发布页，需自己下 dmg 覆盖（与 Linux 版一致）。
- **WASAPI 独占 / 任务栏歌词**：Windows 专属，macOS 上相关设置项不显示。
- 仅提供 **arm64（Apple Silicon）** 构建，未提供 Intel / Universal 包。

## 重新构建

```bash
# 依赖：Xcode Command Line Tools、Node 20+、Rust stable
npm install
npm run tauri build          # 产出 src-tauri/target/release/bundle/{macos,dmg}
```

> 本项目**不使用 GitHub Actions**：上游继承的 `.github/workflows/ci.yml` 已移除，仓库的 Actions 权限也已关闭，构建与测试全部在本机执行（`npm run tauri build` / `cd src-tauri && cargo test --bin rustmusic --locked`）。原因：那份 Windows+Linux CI 会在仓库里生成约 1.6 GB 的 Rust 编译缓存，产生按 GB-月计费的 Actions 存储费用。

本地开发：`npm run tauri dev`。若要对外分发并消除 Gatekeeper 提示，需要 Apple Developer 证书做正式签名 + 公证
（`APPLE_ID` / `APPLE_PASSWORD` / `APPLE_TEAM_ID` 或 `APPLE_API_*`），此时可去掉 `signingIdentity: "-"`。
