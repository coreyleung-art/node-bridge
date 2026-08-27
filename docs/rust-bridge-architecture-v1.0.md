# Rust 统一节点桥（node-bridge）架构规划 v1.0

> 定位：**mac-mini 中枢 × 全部分布式节点的统一通讯层**——不是「给 i9 写的工具」，而是节点网络的基础设施
> 状态：规划稿 · 2026-08-25 · 中枢（fa1f9150）出品
> 约束：**协议 v1.0 通道不变**（不砸旧对讲机）· 升级只加键 · 心跳 60s 唯一存活标准

---

## 一、为什么用 Rust（中枢定位下的理由）

| 维度 | Python 现状（executor+bb+guard 三件套） | Rust 单二进制 |
|---|---|---|
| 部署 | 3 个 .py + 解释器依赖，每节点要装 Python | 1 个 exe/二进制，拷贝即跑 |
| 进程管理 | guard 64 行单独脚本拉起 | 单进程自守护，崩溃即退由平台拉起 |
| 内存 | Python 解释器 ~30-50MB 常驻 | ~2-5MB |
| 平台 | 每节点 Python 版本/编码坑（GBK/UTF-8） | 交叉编译一次，三平台同一代码 |
| 升级 | 覆盖 3 文件，易漏 | 替换 1 文件，签名/版本自检 |
| 稳定性 | 解释器异常/编码崩溃 | 编译期类型安全，panic 即重启 |

**中枢结论**：节点桥是「常驻 × 低资源 × 跨平台 × 免维护」型负载，Rust 是唯一同时满足四者的选择。

---

## 二、架构总览（中枢视角）

```
┌─────────────────────────────────────────────────────┐
│                   黑板 blackboard-server :8792        │
│  tasks/{node}/queue/{ts}  result  notes/{node}/*      │
│  nodes/{node}/heartbeat   data/{node}/*               │
└──────────────┬────────────────────────────────────────┘
               │ HTTP（协议 v1.0 通道不变）
   ┌───────────┼───────────┬───────────┬──────────┐
   ▼           ▼           ▼           ▼
node-bridge  node-bridge  node-bridge  node-bridge
 (mac-mini)   (i9/Windows) (mbp/macOS) (门店/未来)
   ▲           ▲           ▲           ▲
   └─ 统一能力：heartbeat / queue / result / notes ─┘
```

**中枢原则**：
1. **一个二进制，三个平台**——macOS arm64 / Windows x86_64 / Linux x86_64 同一代码交叉编译
2. **协议即契约**——v1.0 通道一字不改，Rust 桥只是换了传输实现
3. **节点自治**——桥不依赖中枢在线，黑板断了重试不崩
4. **动作白名单**——shell/info/scan/dsh/ollama 只增不减；notify 类永远不走 queue
5. **配置驱动**——`--node-id` + `--blackboard`，同一二进制开箱即用任意节点

> **历史问题复盘**：Python 时代 12 个历史问题（离线误判/漏 notes/卡死/乱码/兼容等）已逐条映射到 Rust 桥解决，见 [`rust-bridge-problem-map-v1.0.md`](rust-bridge-problem-map-v1.0.md)——重写不是换语言，是每个坑都有对应解。

---

## 三、模块设计（单二进制内）

```
node-bridge
├── main.rs          # 入口：解析参数 → 启动三线程 → 信号处理
├── config.rs        # --node-id / --blackboard / 间隔 / 白名单
├── bb.rs            # 黑板 HTTP 客户端（GET/PUT/DELETE，显式 Content-Length）
├── heartbeat.rs     # 心跳线程：60s 写 nodes/{node}/heartbeat
├── queue.rs         # 队列线程：2s 轮询 tasks/{node}/queue/ → 执行 → 回报 → 清卡
├── notes.rs         # 消息线程：5s 轮询 notes/{node}/coordinator-*（DSH 智能体对话）
├── actions.rs       # 动作分派：shell / info / scan / dsh / ollama（白名单）
├── exec.rs          # 命令执行：跨平台 shell、超时、UTF-8/GBK 输出归一
└── log.rs           # 轻量日志：stdout + 可选文件滚动
```

### 线程模型（对应 v6 双频轮询 + 教训复盘）
| 线程 | 间隔 | 通道 | 职责 |
|---|---|---|---|
| heartbeat | 60s | `nodes/{node}/heartbeat` | 存活唯一标准 |
| queue | 2s | `tasks/{node}/queue/{ts}` | 取卡投递（含旧 cmd 兼容）|
| worker | 即时 | — | 执行 → 回报 → 清卡（与取卡解耦，scan 不阻塞）|
| notes | 5s | `notes/{node}/coordinator-*` | DSH 智能体对话通道 |

四个线程独立互不阻塞：心跳断了不影响取卡，取卡慢不影响心跳，scan 卡死不阻塞任何——这是 v6 验证 + P6/P10 教训的设计，Rust 里用 `std::thread` + mpsc 即可，无需 tokio。

---

## 四、协议映射（v1.0 一字不改）

| 方向 | 通道 | 方法 | Rust 实现 |
|---|---|---|---|
| 注册 | `nodes/{node}` | PUT | 启动时一次 |
| 心跳 | `nodes/{node}/heartbeat` | PUT | 60s 循环 |
| 取卡 | `tasks/{node}/queue/` | GET | 2s 轮询 |
| 回报 | `tasks/{node}/result` | PUT | 执行后立即 |
| 清卡 | `tasks/{node}/queue/{ts}` | DELETE | 回报后 |
| 旧卡兼容 | `tasks/{node}/cmd` | GET+DELETE | check_legacy_cmd |
| 消息 | `notes/{node}/coordinator-*` | GET/PUT | 5s 轮询 |

**兼容铁律**（继承协议 v1.0 2.4 节）：
- 新能力 → 新键；旧键保持应答
- 通道变更 → 黑板 notes/collab/channel-change 公告 + 双端确认
- 任何一端通道不通 → 查协议文档，不发明新通道

---

## 五、跨平台编译矩阵

| 目标 | 构建机 | 工具链 | 产物 |
|---|---|---|---|
| aarch64-apple-darwin | mac-mini 本机 | cargo build --release | node-bridge-macos-arm64 |
| x86_64-pc-windows-gnu | mac-mini 交叉 | cargo-zigbuild（zig 作 linker/CC） | node-bridge-win-x64.exe |
| x86_64-unknown-linux-musl | mac-mini 交叉 | cargo-zigbuild（全静态） | node-bridge-linux-x64 |

**工具链结论**（调研落链 `research/rust-bridge/rust-cross-compile.md`）：
- 主推 **cargo-zigbuild**：zig 自带 mingw-w64 + glibc/musl，一套工具链同时出 Windows GNU 和 Linux musl；自动过滤 rustc 传给 GNU ld 的专属参数、为 zig≥0.16 补 `-lcompiler_rt`（裸 `linker="zig"` wrapper 无此处理，正式项目不用裸配）
- Windows GNU 目标自 Rust 1.71 默认 self-contained（rustc 自带 mingw 运行库，只需外部 linker 驱动）
- Linux 走 **musl 全静态**：任何发行版拷过去就跑，无 glibc 版本地板问题

**无外部依赖原则**：HTTP 用纯 Rust 栈，TLS 不引 OpenSSL → 交叉编译零负担。
当前 v1.0 实现为 std::net 手写 HTTP（黑板服务完全可控：强制 Content-Length、无 chunked），符合调研给出的手写边界（体积硬指标 + 服务端可控）；二进制 525K < 500KB 边界。将来如需 HTTPS/更复杂 HTTP 语义，按调研升级 ureq 2.x（`default-features=false` 纯 HTTP / 开 `tls` 即 rustls 纯 Rust）。

### 库选型落地对照（调研落链 `research/rust-bridge/rust-node-bridge-libs.md`）

| 层 | 调研推荐 | v1.0 落地 | 对照 |
|---|---|---|---|
| 运行时 | 纯 std::thread + std::net（不要 tokio） | ✅ std::thread 三线程 | 一致 |
| HTTP | ureq 2.x（纯 HTTP）／手写仅当 <500KB 且服务端可控 | ✅ std::net 手写（黑板可控） | 符合手写边界 |
| JSON | serde_json（只用 Value 省编译） | ✅ serde_json::Value | 一致 |
| 日志 | env_logger Target::Pipe 落文件（Windows println 会 panic） | ✅ writeln 忽略错误 + 落 node-bridge.log | 等效加固 |
| 编码 | Windows GBK 需 encoding_rs 解码 | ✅ encoding_rs::GBK 回退 | 一致 |
| 体积 | 无 TLS ≈0.4–0.8MB | ✅ 525K/603K/645K | 达标 |
| TLS 未来 | ureq `tls` feature = rustls（免 OpenSSL） | 待升级 | v2 项 |

**结论**：v1.0 落地与调研推荐栈逐条一致，唯一差异（手写 HTTP vs ureq）正是调研给定的手写适用边界（体积硬指标 + 完全控制服务端，黑板两者都满足）。Windows 无 console 的 println panic 风险已通过 writeln 忽略错误 + 双写文件加固（Windows 服务模式下日志仍可见）。

---

## 六、部署形态（每节点）

```
Windows (i9):  node-bridge-win-x64.exe --node-id i9 --blackboard http://<中枢IP>:8792
               自启：任务计划程序 / NSSM 包成服务（调研确认中）
macOS (mbp):   node-bridge --node-id mbp --blackboard ...
               自启：launchd plist（KeepAlive 崩溃重启）
Linux (未来):  node-bridge --node-id store-01 --blackboard ...
               自启：systemd unit（Restart=always）
```

升级 = 替换 1 个二进制重启，无解释器、无依赖、无三件套。

### 部署模型（调研落链 `research/rust-bridge/rust-daemon-deploy.md`）

**双层：平台服务注册（主）+ 桥内快速失败（辅）**
- 平台管：开机自启 / 崩溃拉起 / 日志归集（Windows NSSM 或 windows-service · macOS LaunchDaemon KeepAlive · Linux systemd Restart=on-failure）
- 桥内只管：panic → 非零退出（101）+ 主循环 stall 检测主动退出让平台重启 + 优雅处理 SIGTERM/SERVICE_CONTROL_STOP
- 不推荐纯内置父子 watchdog 替代平台（丢自启/无登录能力）；禁止桥内 double-fork daemonize（launchd 明确禁止、systemd Type=simple 不需要）
- 日志轮转：桥自滚（大小+时间+保留份数）为唯一事实源 + 平台日志辅助双写——三平台一致解
- Windows 无登录：服务=SCM Session 0 天然无登录；服务账户用 NetworkService（LocalSystem 权限过大）

---

## 七、验收标准

- [x] 三平台编译通过（macOS 本机 ✅ / Windows x86_64-gnu ✅ / Linux musl 全静态 ✅，2026-08-25 zig 0.16 + cargo-zigbuild 0.23.2）
- [x] 心跳 60s 在线判定与 Python 版一致（黑板 nodes/ 视图）
- [x] queue 任务下发→执行→回报→清卡全链路 ✅（2026-08-25 冒烟实测）
- [x] 旧 cmd 通道兼容应答（GET /tasks?node= 列表接口）
- [ ] 崩溃自重启（kill 进程后平台机制拉起）
- [x] 二进制体积可控（macOS 525K / Windows 603K / Linux 645K，< 5MB 目标大幅达标）

### 冒烟测试记录（2026-08-25）

```
node-bridge v1.0.0 start node=test-node bb=http://100.120.203.20:8792 (hb=3s queue=2s notes=3s)
register -> 200
TASK rust-smoke-2 key=tasks/test-node/queue/9999002
DONE rust-smoke-2 ok=true report=200
```
回报实测：`{"task_id":"rust-smoke-2","ok":true,"output":"RUST_BRIDGE_OK\nDarwin","node":"test-node"}`
心跳实测：`nodes/test-node/heartbeat` version 3 连续写入
notes 实测：成功读到 i9 全部对话消息（含 coordinator-test-reply，双向对话闭环确认）

---

## 八、变更记录
- v1.0（2026-08-25）：中枢定位首版规划。单二进制统一三平台节点桥；协议 v1.0 通道不变；三线程模型继承 v6 双频设计。
- v1.0.1（2026-08-25）：代码落地 + macOS 本机冒烟全链路通过（注册/心跳/取卡/执行/回报/清卡/notes 读取）；体积 525K；zig 交叉链接配置就绪。
- v1.0.2（2026-08-25）：**三平台交叉编译完成**——Windows x86_64-gnu（603K PE32+）+ Linux musl 全静态（645K ELF）+ macOS arm64（525K），产物归档 `dist/`；调研三份落链（交叉编译/库选型/部署）；i9 部署指南交付。
- v1.0.3（2026-08-25）：部署调研全量结论固化——双层部署模型（平台服务注册为主 + 桥内快速失败为辅）、NSSM burgerbecky fork、NetworkService 账户、原生 windows-service 为 v2 进阶方案。
- v1.0.4（2026-08-25）：库选型调研全量对照固化——v1.0 落地与推荐栈逐条一致（纯 std 线程/serde_json/encoding_rs/日志加固），唯一差异手写 HTTP 符合调研手写边界；TLS（ureq+rustls）与原生 Windows 服务列为 v2。
- v1.0.5（2026-08-25）：**历史问题复盘落地**——12 个 Python 时代问题全部映射解决（复盘文档 `rust-bridge-problem-map-v1.0.md`）；新增 worker 线程（取卡/执行解耦，scan 卡死不阻塞）与旧 cmd 卡兼容（实测双卡消费）；三平台产物重编译更新。
