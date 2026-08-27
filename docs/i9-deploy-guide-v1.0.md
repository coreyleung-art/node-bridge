# Rust 节点桥（node-bridge）部署指南 · i9 节点

> 交付方：mac-mini 中枢 · 2026-08-25
> 说明：Rust 桥是 Python executor 的替代品（单二进制，免 Python 环境）。**协议 v1.0 通道完全不变**——心跳看 nodes/i9/heartbeat、任务走 tasks/i9/queue/、回报写 tasks/i9/result、消息读 notes/i9/coordinator-*。i9 侧无任何协议感知差异，只是运行载体从 python 换成 exe。

---

## 一、产物（中枢已编译好，直接拷）

| 文件 | 用途 | 体积 |
|---|---|---|
| `node-bridge-win-x64-v1.0.5.exe` | i9（Windows x86_64）主程序 | ~620KB |

传输方式：`scp` 或网盘拷贝到 `E:/My vibe codding/tools/`。**不要用浏览器下载**（会带 MOTW 标记，SmartScreen 拦），scp 传输无此问题。

## 二、启动

```bat
:: 首次：注册 + 常驻运行
E:\My vibe codding\tools\node-bridge-win-x64-v1.0.5.exe --node-id i9 --blackboard http://100.120.203.20:8792
```

参数说明：
```
--node-id i9                 节点标识（必须与黑板现有 i9 一致）
--blackboard http://...:8792 黑板地址
--hb 60                      心跳间隔秒（默认 60，协议标准）
--queue 2                    队列轮询间隔秒（默认 2）
--notes 5                    消息轮询间隔秒（默认 5）
```

## 三、开机自启（二选一）

### 方案 A：任务计划程序（推荐，无需管理员）
```bat
schtasks /Create /TN "node-bridge-i9" /TR "E:\My vibe codding\tools\node-bridge-win-x64-v1.0.5.exe --node-id i9 --blackboard http://100.120.203.20:8792" /SC ONSTART /RU SYSTEM /RL HIGHEST /F
```
- 开机即跑（不依赖登录）
- 崩溃不会自动重启（任务计划语义）→ 用方案 B 更稳

### 方案 B：NSSM 包成服务（崩溃自动重启，推荐）
```bat
nssm install node-bridge-i9 "E:\My vibe codding\tools\node-bridge-win-x64-v1.0.5.exe" "--node-id i9 --blackboard http://100.120.203.20:8792"
nssm set node-bridge-i9 AppExit Default Restart
nssm set node-bridge-i9 Start SERVICE_AUTO_START
nssm start node-bridge-i9
```
- 服务方式：无登录也能跑，崩溃自动拉起
- NSSM 2.27 下载：https://github.com/burgerbecky/nssm（官方 nssm.cc 停更于 2014，用 fork）

> **调研确认**（`research/rust-bridge/rust-daemon-deploy.md`）：NSSM 是把任意 exe 包成服务的零改代码最快路径，服务=SCM Session 0 天然无登录。服务账户建议 NetworkService 或自定义低权限账户（LocalSystem 权限过大）；如需授 SeServiceLogonRight + logs 目录 ACL。

### 方案 C（进阶）：原生 Windows 服务
用 `windows-service` crate 把桥编译成原生服务（SCM 管理、支持 `sc failure` 崩溃重启动作、零外部依赖）。需要改代码加 SERVICE_CONTROL_STOP 处理，是长期最优解；当前先用 B 跑通，需要时中枢升级 v2。

## 四、日志

- 前台运行：日志打 stdout（终端可见）
- 后台/服务：自动写 `node-bridge.log`（当前目录）
- 内容示例：
```
[1787668245] node-bridge v1.0.5 start node=i9 bb=http://100.120.203.20:8792
[1787666359] register -> 200
[1787666359] TASK xxx key=tasks/i9/queue/xxxx
[1787666359] DONE xxx ok=true report=200
```

## 五、验证（启动后 2 分钟内）

| 检查 | 命令/位置 | 预期 |
|---|---|---|
| 心跳在线 | 中枢查 `nodes/i9/heartbeat` | 60s 内新鲜 |
| 任务链路 | 中枢发 `tasks/i9/queue/` 任务 | 秒级消费回报 |
| 消息链路 | 中枢写 `notes/i9/coordinator-*` | 桥日志出现 MSG |

## 六、回滚（出问题恢复 Python 版）

```bat
:: 停服务/任务计划 → 直接跑回 python 版
python E:\My vibe codding\i9-node-agent.py --node-id i9 --blackboard http://100.120.203.20:8792
```
**协议不变，新旧载体可随时切换，不影响黑板侧任何东西。**

---

## 七、变更记录
- v1.0（2026-08-25）：首版交付。单二进制替代 Python executor；协议 v1.0 通道不变；心跳/队列/消息三线程。
- v1.0.1（2026-08-25）：补充调研确认——NSSM 用 burgerbecky fork 2.27（官方停更）；服务账户用 NetworkService；原生 windows-service crate 为进阶方案（v2）。
