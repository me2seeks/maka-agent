# Maka 麦克风权限验证器

这是一个独立的 Electron 43.1.1 诊断程序，用来验证 Maka Desktop Voice
页面所依赖的麦克风权限与采集路径。它只通过无父提交的诊断分支分发，
不属于 Maka 产品源码，也不应合入 PR #1389 或 `main`。

验证器复现当前 Voice 页面的关键操作：

- `getUserMedia({ audio: { channelCount: 1, sampleRate: 48000 } })`
- 使用 `MediaRecorder` 运行约 2 秒
- 只累计内存 Blob 的字节数，随后丢弃并停止所有音轨
- 不保存、不播放、不上传音频
- JSON 不包含设备名称、`deviceId` 或 `groupId`

## 安装和测试

要求 Node.js 22.12.0 或更高版本。

```bash
npm ci
npm test
```

源码启动适合检查 UI 和基本功能：

```bash
# 默认：直接 getUserMedia，不注册 Electron permission handler
npm start

# macOS 对照组：先显式调用 askForMediaAccess，再采集
npm run start:explicit

# Session 层诊断对照组
npm run start:allow
npm run start:deny
```

默认 `direct + default` 才是 Maka 当前 Voice 页面的基线：不先调用
`askForMediaAccess`，也不安装 Session permission handler。

## macOS：必须验证打包后的 `.app`

不要把 `electron .` 的 TCC 行为当作最终结论，因为那可能使用通用
Electron.app 的身份。请在 Mac 本机执行：

```bash
npm ci
npm run package
```

产物位于 `out/Maka Voice Permission Probe-darwin-<arch>/`。脚本只打包
当前机器的系统与架构，不能在 Linux 上生成用于本实验的 macOS/Windows
验证包。先核对稳定 bundle ID 与用途说明已经进入最终 `Info.plist`：

```bash
APP="$(find out -name 'Maka Voice Permission Probe.app' -print -quit)"
/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c 'Print :NSMicrophoneUsageDescription' "$APP/Contents/Info.plist"
codesign --verify --deep --strict --verbose=2 "$APP"
```

bundle ID 应为：

```text
io.github.me2seeks.maka-voice-permission-probe
```

完全退出验证器后，只重置这个应用的麦克风权限：

```bash
tccutil reset Microphone io.github.me2seeks.maka-voice-permission-probe
```

### A. 直接路径（Maka 当前路径）

双击 `.app`，不要添加启动参数：

1. 初始系统权限预期为 `not-determined`；否则先停止并记录实际状态。
2. 点击“运行麦克风自检”。
3. 记录是否出现系统弹窗，并测试“允许”或“不允许”中的一个分支。
4. 复制 JSON；记录菜单栏麦克风指示在停止后的变化（系统可能短暂显示“最近
   使用”，不要把 UI 延迟单独作为失败条件）。

“允许”和“不允许”必须是两个独立实验。每个分支前都要完全退出应用、对
同一个 bundle ID 执行一次 `tccutil reset`、重新启动同一份 `.app`，且中途
不要重新打包。重置后初始状态预期为 `not-determined`；若仍为 `denied`、
`restricted` 或其他值，先停止实验并把该状态写入结果。

### B. 显式系统请求对照组

重新退出、执行同一个 `tccutil reset`，然后从终端启动：

```bash
open -na "$APP" --args --flow=ask-then-capture
```

先点“请求 macOS 麦克风权限”，再点“运行麦克风自检”。拒绝后再次请求通常
不会重复弹窗，需要在“系统设置 → 隐私与安全性 → 麦克风”中修改，并重启
应用后再测。

打包脚本在没有设置签名身份时执行本机 ad-hoc 签名，并验证签名与权限用途
字段；如果设置 `MAKA_MIC_PROBE_SIGN_IDENTITY`，则使用指定的 Developer ID
身份。ad-hoc 产物只适合本机初步诊断，不能替代未来正式签名、公证的 Maka
安装包验收。探针签名只给顶层应用启用 Electron 所需的 JIT 与
`com.apple.security.device.audio-input`；MAS/App Sandbox 产物仍要单独核对
相应 capability。

需要使用已安装的 Developer ID 时：

```bash
MAKA_MIC_PROBE_SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
  npm run package
```

没有真实麦克风时，direct 路径可能在出现 TCC 弹窗前就返回
`NotFoundError`。这不能证明权限机制有问题；应另做
`ask-then-capture` 显式请求对照组。

## Windows：普通 Win32 应用通常没有逐应用弹窗

在 Windows 11 PowerShell 中执行：

```powershell
npm ci
npm test
npm run package
```

运行：

```text
out\Maka Voice Permission Probe-win32-<arch>\Maka Voice Permission Probe.exe
```

这是未签名、非 MSIX 的便携 Win32 文件夹。若 SmartScreen 显示“Windows
已保护你的电脑”，那是代码信誉提示，不是麦克风权限弹窗；MSIX/AppContainer
的 capability 与逐应用权限语义不在本探针范围内。

验证矩阵：

1. 打开“设置 → 隐私和安全性 → 麦克风”。
2. 打开 `Microphone access`。
3. 打开 `Let desktop apps access your microphone`。
4. 运行自检；通常不会出现 macOS 风格的逐应用弹窗。
5. 采集时记录任务栏麦克风图标，并记录停止后的变化；允许少量 UI 延迟。
6. 关闭 `Let desktop apps access your microphone`，完全退出并重启验证器，
   再运行一次。
7. 拔掉或禁用输入设备，再运行一次。

Windows 的普通 Electron/Win32 应用受桌面应用全局麦克风开关控制，不能在
该设置页逐个切换。因此“没有弹窗”本身不是失败条件；应结合系统状态、
任务栏指示和 `getUserMedia` 的实际结果判断。`getMediaAccessStatus` 反映
Win32 全局隐私开关，不证明物理设备存在；页面的 Web Permission 状态属于
Chromium/Electron 层，也不等于 Windows 设置页状态。

## 如何判读错误

- `NotFoundError`：没有可用且满足约束的音频输入设备。
- `NotReadableError`：设备存在并已获授权，但操作系统、驱动、硬件故障或
  其他程序占用导致无法读取。
- `NotAllowedError`：系统隐私设置、用户选择、安全上下文或 Electron
  Session 策略拒绝。

请回传：

1. 验证器 JSON；
2. macOS 首次权限弹窗截图，或 Windows 采集时的任务栏麦克风指示截图；
3. 对应系统麦克风设置页截图；
4. 是否有真实麦克风、是否被其他程序占用。

## 诊断模式

`--session-policy=default` 完全不注册 Electron permission handler，用于基线。
`allow` 和 `deny` 是定位 Session 层问题的对照组：

```bash
# macOS
open -na "$APP" --args --session-policy=allow
open -na "$APP" --args --session-policy=deny
```

```powershell
# Windows
$APP = Get-ChildItem -Recurse out -Filter "Maka Voice Permission Probe.exe" |
  Select-Object -First 1 -ExpandProperty FullName
& $APP --session-policy=allow
& $APP --session-policy=deny
```

`allow` 也不是无条件放行：只有验证器自己的主 frame、当前本地页面、
`media` 权限且仅请求音频时才允许；其他请求全部拒绝。完整权限控制同时
安装 Electron 的 permission check 与 request handler。

## 参考

- [Electron systemPreferences](https://www.electronjs.org/docs/latest/api/system-preferences)
- [Electron Session permissions](https://www.electronjs.org/docs/latest/api/session)
- [Electron security checklist](https://www.electronjs.org/docs/latest/tutorial/security)
- [Apple：Requesting authorization for media capture](https://developer.apple.com/documentation/bundleresources/requesting-authorization-for-media-capture-on-macos)
- [Apple：NSMicrophoneUsageDescription](https://developer.apple.com/documentation/BundleResources/Information-Property-List/NSMicrophoneUsageDescription)
- [Microsoft：Windows camera, microphone, and privacy](https://support.microsoft.com/en-US/Windows/privacy/windows-camera-microphone-and-privacy)
- [W3C Media Capture and Streams](https://www.w3.org/TR/mediacapture-streams/)
