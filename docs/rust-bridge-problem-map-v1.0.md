# node-bridge Rust 重写 · 历史问题复盘与对应解决（v1.0）

> 目的：**重写不只是换语言**——把 Python executor 时代踩过的每一个坑，分析清楚根因，并在 Rust 桥里给出对应解决。每条都有「问题 → 根因 → Python 时代的应对 → Rust 桥的解决 → 验证状态」。
> 出品：mac-mini 中枢 · 2026-08-25 · 配套架构文档 `rust-bridge-architecture-v1.0.md`

---

## 一、问题总览（12 个历史问题，全部有对应解决）

| # | 问题 | 类别 | Rust 桥解决 | 状态 |
|---|---|---|---|---|
| 1 | i9「离线」误判 | 存活判定 | 心跳 60s 唯一标准 + 独立线程 | ✅ 已落地 |
| 2 | 秒级轮询漏 notes 键 | 通道 | 三线程独立，notes 专属线程 | ✅ 已落地 |
| 3 | Windows python 命令被拒 | 环境 | 单二进制免解释器 | ✅ 根除 |
| 4 | C 盘根目录写被拒 | 权限 | 工作目录自由指定 | ✅ 根除 |
| 5 | 复杂引号命令不稳定 | 执行 | Command 直接传参 + worker 解耦 | ✅ 已落地 |
| 6 | scan 大目录卡死进程 | 执行 | depth/条数双限 + worker 线程 | ✅ 已落地 |
| 7 | 通道混乱/对空气说话 | 协议 | 协议 v1.0 固化，只实现约定通道 | ✅ 已落地 |
| 8 | 旧 cmd 卡兼容 | 协议 | check_legacy_cmd 保持应答 | ✅ 已落地（实测）|
| 9 | 任务回报丢失/乱码 | 传输 | 显式 Content-Length + GBK→UTF-8 | ✅ 已落地 |
| 10 | 心跳被任务阻塞 | 架构 | 四线程（心跳/取卡/worker/notes）| ✅ 已落地 |
| 11 | 密钥明文风险 | 安全 | 桥零硬编码，密钥走环境/外部 | ✅ 已落地 |
| 12 | 部署三件套易漏 | 运维 | 单二进制 + 三平台部署指南 | ✅ 已落地 |

---

## 二、逐条复盘（问题 → 根因 → 解决）

### P1. i9「离线」误判（最痛的教训）
- **问题**：中枢判定 i9 离线，实际在线。反复误判多次。
- **根因**：
  1. 按 UTC 解析本地时间——黑板时间戳是本地时区，解析错位
  2. 看错指标——用 tasks/cmd、node-agent 进程判断存活，而任务卡本来就可以长期为空
- **Python 时代应对**：人工修正为「heartbeat 60s 新鲜=在线」唯一标准（协议 v1.0 §1.1）
- **Rust 桥解决**：
  - `heartbeat_loop` 独立线程，严格 60s 写 `nodes/{node}/heartbeat`
  - 存活判定只看 heartbeat，不看任务卡/进程（`main.rs` 注释明确）
  - 时间戳用本地 ISO 秒（`now()`），不引入 UTC 解析歧义
- **验证**：✅ 冒烟实测心跳 version 连续递增

### P2. 秒级轮询漏 notes 键
- **问题**：中枢秒级轮询 i9 的 result+cmd，收不到 i9 的 notes 消息——「对空气说话」
- **根因**：轮询只覆盖 `tasks/{node}/result` + `cmd`，而 i9 消息写在 `notes/i9/*`，监控路径不全
- **Python 时代应对**：i9-poll-seconds.py v2 加 notes/i9 监控
- **Rust 桥解决**：三线程独立——`heartbeat_loop`（60s）/ `queue_loop`（2s）/ `notes_loop`（5s），notes 专属线程读 `notes/{node}/coordinator-*`，结构上杜绝漏键
- **验证**：✅ 冒烟实测读到 i9 全部对话消息（coordinator-test-reply 闭环确认）

### P3. Windows python 命令被拒
- **问题**：i9 上 `python` 命令报「拒绝访问」
- **根因**：Windows PATH 首个命中是 WindowsApps 的 PythonManager 存根（受 Store 限制），真解释器在 `py` launcher
- **Python 时代应对**：改用 `py` launcher
- **Rust 桥解决**：**根除**——单二进制不依赖任何解释器，不存在 PATH 存根问题
- **验证**：✅ 交叉编译产物在无 Python 环境可跑（Windows 部署指南）

### P4. C 盘根目录写被拒
- **问题**：i9 写 `C:\` 根目录被拒
- **根因**：Windows 系统盘根目录权限限制
- **Python 时代应对**：改用 `%USERPROFILE%`
- **Rust 桥解决**：**根除**——桥无固定写路径，工作目录由启动方式决定（推荐 `E:/My vibe codding/tools/`）；日志写当前目录 `node-bridge.log`
- **验证**：✅ 部署指南明确路径约定

### P5. 复杂引号命令不稳定
- **问题**：i9 executor 执行带引号/嵌套引号的命令多次 `ok:False`
- **根因**：Python `subprocess.run(shell=True)` 的引号传递层级问题 + 中间层转义
- **Python 时代应对**：用 echo 重定向写文件再执行
- **Rust 桥解决**：
  - `Command::new(program).arg(flag).arg(cmd)`——cmd 作为**单个参数**传给 `cmd /C` / `sh -c`，不经 Python 字符串再解析层
  - worker 线程执行，异常不炸取卡循环
- **验证**：✅ 冒烟（含 echo 命令）ok=true；遗留：极复杂引号仍建议写脚本文件执行（文档注明）

### P6. scan 大目录卡死进程
- **问题**：i9 scan-002 任务（3 个大 .vue 文件）卡死 executor 进程，需清任务卡 + 恢复
- **根因**：`os.walk` 无深度/条数上限，大目录递归吞掉内存/时间，进程无响应
- **Python 时代应对**：人工清卡 + i9-recovery-test
- **Rust 桥解决**（双重防护）：
  1. **硬限制**：`walk()` 深度上限（默认 2，可配）+ 条目数上限 2000 截断（`actions.rs`）
  2. **架构解耦**：worker 线程执行 scan，卡死只影响该 worker 的任务，心跳/取卡线程不受影响——进程不会整体卡死
- **验证**：✅ 代码级防护 + worker 架构实测

### P7. 通道混乱/对空气说话
- **问题**：多次升级迭代「砸了旧对讲机」——新代码不认旧通道，消息发出去没回应
- **根因**：每次迭代发明/更换通道，无固化契约
- **Python 时代应对**：协议 v1.0 固化（`node-channel-file-protocol-v1.0.md`）：只加键不换通道
- **Rust 桥解决**：桥**只实现**协议 v1.0 约定通道（heartbeat/queue/result/notes），不发明任何新通道；通道变更需黑板公告+双端确认
- **验证**：✅ 代码对照协议文档逐通道核对（见 §三）

### P8. 旧 cmd 卡兼容
- **问题**：任务卡协议从 `tasks/{node}/cmd`（单卡）演进到 `tasks/{node}/queue/{ts}`（队列），旧卡若没人应答就丢
- **根因**：协议演进时旧通道被新实现忽略
- **Python 时代应对**：check_legacy_cmd 兼容
- **Rust 桥解决**：`queue_loop` 同时识别 `k.ends_with("/cmd")`（旧卡）与 `k.contains("/queue/")`（新卡），都走 worker 执行回报——**实测双卡并发消费成功**
- **验证**：✅ 2026-08-25 实测 `rev-legacy` + `rev-queue` 双卡回报，cmd 卡清空

### P9. 任务回报丢失/乱码
- **问题**：① Windows urllib PUT body 偶发丢失（Content-Length 未显式设置）；② 命令输出 GBK 乱码进黑板
- **根因**：① Python urllib 对 PUT body 的 Content-Length 处理不可靠；② Windows 中文环境默认 GBK 编码
- **Python 时代应对**：http.client 显式 Content-Length + locale 检测解码
- **Rust 桥解决**：
  - `bb.rs` 手写 HTTP 请求**始终显式 Content-Length**（字符串长度，无隐式转换）
  - `exec.rs` 输出解码：UTF-8 优先，失败回退 `encoding_rs::GBK`（Windows 中文零乱码）
- **验证**：✅ 冒烟含中文场景设计；编码路径已实现

### P10. 心跳被任务阻塞
- **问题**：单线程架构下，长任务（scan/大命令）执行期间心跳停了 → 误判离线
- **根因**：心跳与任务执行同一线程（或 Python 版 poll 内串行）
- **Python 时代应对**：心跳/轮询分离（node-executor 设计要点 5）
- **Rust 桥解决**：**四线程架构**——heartbeat / queue（取卡）/ worker（执行）/ notes 各自独立；任何线程卡死不影响其他
- **验证**：✅ 架构级保证（§三 线程模型）

### P11. 密钥明文风险
- **问题**：activity AK/SK + flowercheck SSH root + CLphoneuse ARK/GLM 等 4 处密钥明文
- **根因**：代码硬编码
- **Python 时代应对**：代码级清理 + 用户云控制台轮换（待办）
- **Rust 桥解决**：桥**零硬编码密钥**（`grep` 验证无 AK/SK/secret/token/password/api_key）；桥只做节点通讯，不持有任何业务密钥
- **验证**：✅ 代码 grep 无命中

### P12. 部署三件套易漏
- **问题**：executor + bb + guard 三个 .py 文件，升级覆盖易漏、每节点要装 Python
- **根因**：多文件 + 解释器依赖
- **Python 时代应对**：手动保证
- **Rust 桥解决**：**单二进制**（525K-664K），拷贝即跑；三平台产物 + 部署指南（NSSM/launchd/systemd）
- **验证**：✅ 三平台交叉编译成功

---

## 三、代码级落实对照（每条解决在哪个文件）

| 解决 | 文件/函数 | 说明 |
|---|---|---|
| 心跳独立线程 | `main.rs::heartbeat_loop` | 60s 写 heartbeat，与任务解耦 |
| notes 专属线程 | `main.rs::notes_loop` | 5s 读 notes/{node}/coordinator-* |
| 取卡/执行解耦 | `main.rs::queue_loop` + `worker_loop` | mpsc channel，producer-consumer |
| 旧 cmd 兼容 | `main.rs::queue_loop` | `ends_with("/cmd")` 识别旧卡 |
| 显式 Content-Length | `bb.rs::request` | 手写 HTTP，长度显式 |
| GBK→UTF-8 | `exec.rs::decode_utf8` | encoding_rs GBK 回退 |
| scan 深度/条数限制 | `actions.rs::walk` | max_depth + 2000 截断 |
| 未知 action 不退出 | `actions.rs::execute` | `other => (false, ...)` 回报错误不崩 |
| 命令单参数传递 | `exec.rs::run_shell` | Command arg 传参，不经字符串再解析 |
| 零密钥硬编码 | 全部 src/ | grep 验证无命中 |
| 日志防 panic | `main.rs::log` | writeln 忽略错误 + 双写文件 |

---

## 四、验证记录

| # | 验证项 | 结果 | 时间 |
|---|---|---|---|
| 1 | 心跳/注册/回报/清卡全链路 | ✅ | 2026-08-25 22:00 |
| 2 | notes 读取 i9 消息 | ✅ | 2026-08-25 22:00 |
| 3 | 旧 cmd + 新 queue 双卡并发 | ✅ | 2026-08-25 22:30 |
| 4 | worker 解耦（scan 不阻塞取卡） | ✅ 架构保证 | 2026-08-25 22:30 |
| 5 | 三平台交叉编译 | ✅ | 2026-08-25 22:35 |

---

## 五、遗留与边界

- **极复杂引号命令**：仍建议节点侧写脚本文件再执行（文档注明，非桥缺陷）
- **崩溃自重启**：需 i9 侧切到服务方式（NSSM）后回测
- **日志轮转**：v2 候选（flexi_logger 自滚）
- **密钥轮换**：4 处历史明文需用户云控制台轮换（桥已零硬编码，但旧环境残留要清理）

---

## 六、变更记录
- v1.0（2026-08-25）：首版复盘。12 个历史问题全部映射到 Rust 桥解决；新增 worker 线程（P6/P10）与旧 cmd 兼容（P8）两处代码改进并实测。
