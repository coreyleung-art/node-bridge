# node-bridge — 跨设备桥（常驻）

跨设备桥常驻进程：心跳上报、任务队列、worker 执行、notes 同步、outbox 投递 + LLM 执行器。

## 功能

- **心跳**：定期向黑板写 `nodes/<node>/heartbeat`（60s 默认）
- **任务队列**：从黑板 `tasks/<node>/queue/*` 取卡执行，结果写 `tasks/<node>/result`
- **notes 同步**：黑板 notes → 本地 inbox 落盘
- **LLM 执行器**（v2）：三因子门禁（flash/日配额/互斥锁/退避重试/dead-letter/台账）
- **identity 支持**（v1.3.0，P1-3b）：启动自动注册 device_id + token

## 启动

```bash
# macOS（launchd 托管示例）
node-bridge --node-id mac-mini --blackboard http://127.0.0.1:8792 --hb 60 --queue 2 --notes 5

# Windows（计划任务）
node-bridge.exe --node-id i9 --blackboard http://100.120.203.20:8792 --hb 60 --queue 2 --notes 5

# identity（v1.3.0+）
node-bridge --node-id mbp --identity ~/.dsh/identity-mbp.json
```

## 参数

| 参数 | 默认 | 说明 |
|------|------|------|
| --node-id | node | 节点名（mac-mini/mbp/i9）|
| --blackboard | http://100.120.203.20:8792 | 黑板地址 |
| --hb | 60 | 心跳间隔（s）|
| --queue | 2 | 队列轮询间隔（s）|
| --notes | 5 | notes 轮询间隔（s）|
| --no-llm | false | 禁用 LLM 执行器 |
| --identity | 无 | identity.json 路径（v1.3.0）|
| --token | 无 | 黑板认证 token（预留）|

## 三平台产物

- `node-bridge-macos-arm64-v1.3.0`（GitHub Release）
- `node-bridge-win-x64-v1.3.0.exe`（GitHub Release）
- `node-bridge-linux-x64-v1.3.0`

## 版本历史

见 CHANGELOG.md（v1.2.0 LLM 执行器 v2 / v1.3.0 identity 支持）

## 设计要点

- 独立 launchd 进程（不随 CLD 重启）
- Rust 单二进制自包含（无 openssl 依赖，Windows 可用）
- identity：启动时读/生成 identity.json（device_id + token），注册节点带 device_id
