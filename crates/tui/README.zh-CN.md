<!--
  Licensed to the Apache Software Foundation (ASF) under one
  or more contributor license agreements.  See the NOTICE file
  distributed with this work for additional information
  regarding copyright ownership.  The ASF licenses this file
  to you under the Apache License, Version 2.0 (the
  "License"); you may not use this file except in compliance
  with the License.  You may obtain a copy of the License at

      http://www.apache.org/licenses/LICENSE-2.0

  Unless required by applicable law or agreed to in writing,
  software distributed under the License is distributed on an
  "AS IS" BASIS, WITHOUT WARRANTIES OR CONDITIONS OF ANY
  KIND, either express or implied.  See the License for the
  specific language governing permissions and limitations
  under the License.
-->

[English](./README.md)

# Maka TUI：M0 交互原型

用户能否一边阅读旧 thinking，一边编辑输入并接收新输出？这个 Cargo workspace 成员提供独立演示和显式 TS Host 会话模式，不替换现有 TUI。

状态：**Current，源码原型；M0 尚未完成**。下文区分已实现入口和待完成能力。

## 不改变默认 TUI 的运行方式

`npm run dev:maka-tui` 现在启动下文的独立真实模型 Host，不再默认进入演示；`-- --help` 查看其他模式。若只看界面，运行 `npm run dev:maka-tui:demo`；演示需要 Rust 1.98.0 和 Cargo 依赖，不需要先安装 npm 依赖。也可直接运行：

```sh
cd crates/tui
cargo run --locked -j 1 -- --demo-chat
```

演示要求 stdin/stdout 都连接交互终端。不带参数或使用 `--help` 只显示帮助，不进入终端模式。演示不连接 Host、不运行工具、不发送提示词、不读取凭据，也不持久化输入；退出会立即关闭演示，内存中的输入不会持久化。现有 `maka` 命令保持不变。

## 体验边读边输入

`--demo-chat`（也是 `npm run dev:maka-tui:demo` 的入口）打开安静的聊天壳，不自动生成回放消息或权限请求。Enter 请求发送，但演示尚未连接 Host，会保留输入并明确提示无法发送。Shift+Enter 或 Ctrl+J 换行；预览不发送、不保存输入。`--demo` 保留下面的旧压力回放 fixture。

空草稿输入 `/`，或任何时候按 Ctrl+P，打开按需出现的命令列表。支持英文名称和中文关键词搜索、方向键选择、Enter 确认和鼠标左键点击；Esc 返回原草稿和阅读位置。输入 `//` 可在空草稿中开始普通路径。粘贴的斜杠文本不会执行命令；弹层内粘贴不会修改草稿。列表使用 Ratatui `List/ListState`；目前只展示已实现的帮助、回到最新、思考展开及本地预览，不提供假的会话／设置／插件入口。

新聊天壳和命令列表默认使用简体中文，文案集中在 `src/i18n.rs`；命令标识保持稳定，旧回放／IPC 诊断仍有英文，目前没有语言切换。搜索支持左右移动、Home/End、Delete/Backspace 和安全粘贴（换行／Tab 转为空格，不执行命令），最多 128 字节；列表支持鼠标滚轮。12 列 × 4 行起提供紧凑查询／结果视图，更小窗口暂停查询输入，Esc 始终可返回。长查询按字符簇水平滚动。

fixture 包含五段长 thinking、独立的工具失败状态和有上限的流式输出。向上翻页、保持 Draft 键盘焦点、粘贴多行文本并改变窗口宽度：阅读位置仍锚定同一源内容块，编辑草稿不跟随新输出。Ctrl+T 统一折叠当前与未来的 thinking 正文，同时保留被隐藏正文的锚点。

| 操作 | 按键 |
| --- | --- |
| 编辑时翻阅历史 | PgUp/PgDn；历史区鼠标滚轮不切换焦点 |
| 切换键盘焦点 | F6；Esc 返回 Draft |
| 历史聚焦时导航 | Up/Down、j/k、PgUp/PgDn；Home 到开头 |
| 明确跟随最新输出 | F4；或历史聚焦时 End |
| 展开／折叠所有 thinking | Ctrl+T |
| 换行；按视觉行移动 | Enter；Draft 中 Up/Down |
| 撤销／重做一次编辑或整块粘贴 | Ctrl+Z / Ctrl+Y |
| 仅报告草稿大小，不发送 | F5 |
| 暂停／继续；停止 fixture 回放 | F2；Ctrl+C |
| 查看演示权限请求 | F3 |
| 帮助；退出 | F1；Ctrl+C（紧凑提示显示为 ^C） |

回放运行时 Ctrl+C 先停止；空闲后 Ctrl+C 立即退出。Ctrl+Q 不执行动作。
上表保留旧回放的操作，不是聊天界面的发送键约定。

普通滚动到最后一页仍保持 READING；只有明确回到最新才重新进入 LIVE。“+N”表示阅读历史期间发生的内容更新次数，不是未读消息数。

粘贴内容只作为文本，包括斜杠命令。CRLF 归一化，不支持的控制字符导致整次输入拒绝。移动和删除以完整字符簇为单位，tab 使用四列制表位。视觉导航与光标定位共享布局；恰好填满的逻辑行末尾增加一个空插入行，但不向草稿增加换行符。尚未实现选择、复制和结构化附件。

这些粘贴／Repeat 保证仅适用于后端能够识别的事件。如果后端明确不支持 bracketed paste 或增强键，就跳过该可选模式，也不释放未取得的模式。INPUT! 标记和 F1 帮助会说明限制：旧式字符流无法证明粘贴边界或区分重复按下。其他初始化错误仍导致失败退出。接受后续编辑或出现更新的操作通知时清除旧编辑错误，避免旧拒绝提示遮住新请求或停止结果。

演示请求在回放第 12 tick 到达、第 60 tick 失效，不抢键盘焦点。F3 明确打开后，Enter 默认拒绝，只有无修饰键的单次 “a” 按下才能记录本地允许。过期或被替代的请求不能接收回答。弹层消费输入，包括 Ctrl+C；先按 Esc 关闭，再执行后台动作。这不能证明真实 Host 权限交互已经安全。

小于 24 列或 7 行时，输入区取代历史区并拥有键盘焦点，禁止导航不可见的历史。至少四行时保留焦点、当前状态、输入与退出提示。弹层小于 24 列或 4 行时提示调整尺寸，并禁用允许操作；收到 resize 就撤销该动作资格，不等待下一帧。Esc 仍可返回。1×1 空间无法容纳完整指引。调整尺寸不会删除输入或改变保存的阅读锚点。帮助内容无法完整容纳时明确提示调整尺寸。

## 连接已有 TS Host 会话

本机已有更新 epoch 的 Host 也可以测试，不能把旧后端指向其数据目录或跳过兼容性检查。准备下文 TS 构建后，从这个 worktree 根目录运行：

```sh
npm run dev:maka-tui -- --isolated-real
```

这是使用真实模型的入口。它只在 `crates/tui/.development/epoch154` 创建／连接有本 launcher 标记的独立生产 Host；目录已忽略，不提交 Git。首次在终端选择服务类型、Base URL、模型 ID，并隐藏输入 API key，通过 Host 保存连接。不会读取现用 Host 的配置或会话；隔离 candidate 禁止从环境变量导入 OpenAI、Anthropic、DeepSeek bootstrap 凭据。不会自动选择免费模型。保存连接时可能查询所选服务商的模型目录；进入聊天后明确发送才调用模型。

再次运行复用独立连接与历史；退出保留数据，本次启动的 Host 随 launcher 退出关闭。该目录包含凭据和聊天数据，不要分享或提交。已有 Host 若不兼容则拒绝，不自动升级。自动化测试使用 FakeBackend，不调用真实模型；真实服务商需要在本机配置后验证。工具等待授权时使用 F3 打开请求。

若只做无凭据的协议／交互测试，可运行：

```sh
npm run dev:maka-tui -- --isolated
```

这会启动临时 epoch-154 TS Host，使用 FakeBackend 而非真实模型。真实协议、会话存储和流式收发均经过 Host；没有模型调用费用，不复制现用连接、密钥或会话。正常退出会关闭本次测试 Host 并删除其临时数据，不影响现用 Host。当前在 Linux 验证；这不是可保留历史的真实模型开发环境。

如已有独立配置好的 epoch-154 Host，可使用：

```sh
npm run dev:maka-tui -- --root /已有/state-root
npm run dev:maka-tui -- --root /已有/state-root --new
npm run dev:maka-tui -- --root /已有/state-root --session 会话ID
```

第一条在进入聊天前提供分页会话选择（序号选择、n 新建、p 下一页、q 退出）；聊天内 `/sessions` 尚未实现。新建会话使用当前工作目录、Host 默认模型及 ask 权限模式；缺少模型配置时由 Host 拒绝，不复制全局配置。已有 Host 模式不会启动、升级、重启或关闭 Host；不兼容时明确报错，不退回演示。开发命令每次以单 job 增量构建 Rust。

先按下文准备固定 head 的 TS 依赖与构建，再运行：

```sh
cargo build --locked -p maka-tui -j 1
target/debug/maka-tui --connect node crates/tui/companion/session-host.ts /显式/state-root 已有会话ID
```

此入口只连接已有 epoch-154 Host，不自动启动、升级、创建会话或重连。
Node 与脚本路径必须可信。TS Client 负责完整订阅、历史分页和协议校验，Rust 负责终端与输入；
共享 Rust 协议仍用于原生目录探针，尚未替代完整 TS 订阅客户端。
Enter 发送文本，Shift+Enter／Ctrl+J 换行；Host 接纳回执到达且输入未被修改时才清空输入。
运行时 Ctrl+C 请求停止，等 Host 确认空闲后再次按下才退出；空闲时直接退出，没有丢弃确认框。
输入只在进程内保留，退出后不保存。断线后未确认发送的结果未知，不自动重发。

原生 `--connect` 仍是显式选定一个已有会话的低层入口；开发 launcher 提供启动阶段的选择和创建。聊天内不包含会话切换、附件、插件 UI 或自动恢复。

### 权限、Agent 提问与表单

Host 请求到达后仅显示提示，不抢走聊天输入焦点。F3 或命令列表中的 `/requests` 打开请求；Esc 暂时返回聊天并在进程内保留填写内容，不发送取消回答。授权默认拒绝；批准必须先翻到完整审查内容末尾，再明确选择。epoch 154 的沙箱扩展与客户端能力授权会影响当前会话后续调用，因此按钮明确标为“批准会话范围”，不是“允许一次”。表单支持文本、数字、整数、布尔、单选和多选，Agent 提问还支持自由回答。方向键移动、空格选择、Enter/Tab 下一步，最后单独确认提交。文本使用 Shift+Enter/Ctrl+J 换行；F2 可在填写途中取消回答。按钮支持鼠标点击；选项目前使用键盘操作。

回答必须经过 Host 查询、校验和回执；等待期间不能重复提交，过期请求立即禁用。窗口至少 40×10 才启用交互动作。输入及回答有大小上限，无法完整呈现的授权内容不提供批准操作。旧版 permission 和未知请求类型明确标为不支持，不能套用通用授权。

MCP 或插件若使用 Host 已有的声明式表单，可复用同一界面；这不是 TUI 插件执行器，也不代表 MCP 全链路已完成验收。当前 Companion 未注册 MCP 能力提供端，不会自行加载 MCP 服务。请求执行与持久化仍由 Host 负责。

多个请求显示数量并逐个处理；提交或取消当前请求后，再用 F3 打开下一项。工具输出可显示明确标记的截断／控制字符清理预览，原文留在 Host；授权审查不会使用这种截断预览。

发送文本及正文单块上限 16 KiB，呈现历史最多 128 块／256 KiB；历史超额会中止连接并提示，不能视作完整历史。
Linux 隔离 Host + FakeBackend PTY 测试覆盖真实收发、提问回答、沙箱请求拒绝与停止；不代表真实模型、IME 或跨平台验收。

## 锁定 TS Host 的只读探针

项目现属于根 Cargo workspace，使用根目录的 Cargo.lock 和 toolchain。
协议、传输及其 runtime/presentation 类型依赖来自固定提交
`98f40e46a855bb0817e563a878424559b5a42b3f`，来源范围记录于
`crates/upstream.json`。

Rust 原生客户端已可不经过 Node Companion 读取一页会话目录：

```sh
cargo build --locked -p maka-tui -j 1
target/debug/maka-tui --list-sessions /显式路径/registration.json
```

该开发者命令将实时握手与指定注册文件的 root、Host epoch、协议和 composition
逐项核对，网络过程总计 5 秒超时；输出是私密诊断 JSON。
不会启动/升级 Host 或写入会话状态；交互收发使用上面的 `--connect` 入口。
先构建二进制再运行 `test:maka-tui-host`；核心集成测试会让 Rust 连接临时 TS Host。
仅向 ready 的 Host 查询；其他已接受握手的生命周期状态会提示尚未就绪，不发送查询。
调用方必须信任注册文件及其 endpoint：身份字段一致不等于所有权认证。
可信 control namespace 发现及 endpoint 所有权检查尚未实现。
共享 crates 仅引入源码，未迁入上游 conformance 测试集，不代表完整协议覆盖。

后端源码固定在 `05d4d8ec45524f119c2dc1e4eae21826d6f90e7a`，
协议版本为 0、compatibility epoch 为 154，记录于 `host-baseline.json`。
原有 echo 仍然只是独立本地探针，不是 Host 接纳凭证；真实收发只在显式 `--connect` 模式启用。

从仓库根目录依次运行，每次只运行一条，不做整个 workspace 构建：

```sh
npm ci --ignore-scripts --no-audit --no-fund
node scripts/apply-dependency-patches.mjs
node scripts/sync-model-metadata.mjs
NODE_OPTIONS=--max-old-space-size=1536 node node_modules/typescript/bin/tsc -b packages/mcp packages/runtime-host
npm run typecheck:maka-tui-companion
npm run test:maka-tui-host
node crates/tui/companion/probe-host.ts --root /显式指定的/已有/state-root
```

最后一条只连接已运行的本地 Host，读取一页会话目录（最多 32 条），
关闭自身连接后输出 JSON。整份 JSON（含名称、ID、cursor 和 hostEpoch）
均为私密诊断输出，分享前请脱敏。目录查询设置 5 秒超时，并等待连接清理完成。
它不会创建 State Root、启动/重启/升级/停止 Host、发送消息或修改已读状态。
测试仅创建和清理自己的临时 FakeBackend Host，不使用用户的真实会话。

启动探针时检查后端已跟踪源码与固定提交的差异，并核对编译产物导出的协议常量。
仅修改 TUI 的提交可以继续前进，不要求分支 HEAD 永远停在此提交。
这不是完整的产物新鲜度检查，也不能证明运行中 Host 的精确源码 SHA；
连接身份与协议/epoch/composition 兼容性由官方客户端验证。
后端实验后需要重新构建，不能混用其他 worktree 的 `dist`，也不会自动更新基线。

## Rust 权责与工程约定

对齐 feat/runtime-host-rust，作为根 Cargo workspace 的成员：edition 2024、Rust 1.98.0、入库 Cargo.lock、Apache-2.0，以及 `publish = false`。固定 Ratatui 0.30.2 和 Crossterm 0.29.0，关闭不需要的 Ratatui 可选功能。生产代码禁止 unsafe，公共 API 必须有文档；关闭开发／测试调试信息以减少本地构建和产物开销。

Composer 拥有原子编辑、revision 和有界撤销；Transcript 拥有源锚点、折行与跟随策略；App 拥有统一输入路由和确定性 fixture，不拥有执行事实。私有终端 scope 拥有模式获取与清理：成功取得的模式只逆序释放一次，恢复原 panic hook，并保留操作与恢复的双重错误。正常结束、错误和 unwind panic 属于支持路径；SIGTERM、挂起／恢复、外部程序交接、abort 和 SIGKILL 不在清理保证内。

当前编辑核心刻意保持小范围：已评估控件按 Unicode scalar 删除的行为不能证明符合字符簇契约。选择、IME／终端模拟器行为，以及编辑控件复用或适配仍是 M0 决策，原型不证明生产环境应完整自研编辑器。

## 单任务验证

在仓库根目录**依次**执行以下命令，不同时运行多个任务。验证这个 crate 不需要全仓 npm 构建。

```sh
npm run format:maka-tui:check
npm run test:maka-tui
npm run lint:maka-tui
npm run build:maka-tui
python3 crates/tui/tests/pty_smoke.py
cargo deny --manifest-path crates/tui/Cargo.toml --locked --config deny.toml check licenses sources
```

构建、测试、lint 入口固定 `-j 1`，测试另加 `--test-threads=1`。单 Cargo job 不等于硬内存上限；应观察可用内存和活跃换页，持续压力上升时暂停。本 Linux 机器第一次依赖编译加回归测试约 32 秒，命令报告峰值 RSS 约 329 MiB，依赖已预先下载。这不是整机峰值，也不是输入延迟测量。

PTY 脚本不触发构建，只使用 Linux Python 标准库，运行现有 debug 二进制及一个单独标记 ignored 的 panic 测试产物。TestBackend 检查用户可见的阅读、焦点、草稿和退出提示；PTY 检查 termios、协议清理与真实输入送达，不验证终端模拟器画面或 IME。已定义 Linux/macOS/Windows 独立源码 admission 工作流；文件存在不代表远端任务已经通过。

2026-09-12 本地验证：76 项普通 Rust 测试通过（library 64、binary 12）；平时忽略的 panic 测试在 PTY 中单独通过。两条 PTY 场景、格式检查、禁止警告的 Clippy、debug 构建、依赖许可／来源检查均通过。两名独立 gpt-5.6-sol high 审查者并行检查 Rust／终端权责和 UX，不运行构建；发现的问题转化为回归测试并修复。这是源码原型审查，不是发布批准。

## 上限与后续门槛

2026-09-16 聊天壳／命令列表切片验证：84 项 library 测试、42 项 binary 测试通过，另有一项 panic 测试在 PTY 中单独通过；格式、Clippy（禁止警告）和 debug 构建通过。四条 Linux PTY 场景覆盖中文聊天入口、查询与字面斜杠、草稿编辑／退出、panic 清理和 Companion 故意不确认请求时的协议错误退出／终端恢复。所有 Cargo 任务串行使用 `-j 1`，测试使用 `--test-threads=1`。两个 Luna max 子代理只读审查代码与 UX；这些检查不证明真实 Host、终端模拟器／IME 或跨平台发布可用。

若旧测试产物使 PTY 脚本无法唯一定位 panic 测试，请把最新 `cargo test` 输出中的 `src/main.rs` 测试二进制绝对路径通过 `MAKA_TUI_PANIC_TEST_BINARY` 传入；不要猜测或删除其他产物。

历史最多接纳 128 个块，单正文 16 KiB，含标题的总源文本 256 KiB，单标题最多 256 字节；超限明确拒绝，不静默截断。演示草稿上限 64 KiB。撤销最多保留 128 次事务、4 MiB 文本快照；单次事务超过撤销预算时切断历史，这是通用模块策略，演示草稿上限下不会触发。

驻留历史重排、草稿布局均同步从有界源文本重算，包括逐字符簇光标位置。源文本预算不等于实际堆内存，也不保证帧延迟。这还不是分页／虚拟化生产阅读器，更不是 50 MiB 历史性能证明。

M0 仍需拥塞和生命周期验证、复制／IME 与实际终端检查、跨平台验证和延迟／资源测量。后续阶段还包括会话切换、附件、断线恢复、Markdown／搜索、完整命令与工作流、打包。目前没有新增依赖 notices 产物或发布二进制集成。不能据此切换默认 TUI 或宣称重写完成。
