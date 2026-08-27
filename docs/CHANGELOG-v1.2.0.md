# node-bridge v1.2.0 · LLM 执行器三因子门禁版

> 日期：2026-08-27 · 上游：8/26 成本事故（MBP pro 模型 + 循环 bug → ¥174）后的修复版
> 解决：设备自主互通「无需人工监督」的最后一块拼图——LLM 执行器可从 --no-llm 禁用态安全启用

## 变更点

### 1. LLM 执行器三因子门禁（src/llm.rs v2）
| 门禁 | 实现 | 防什么 |
|---|---|---|
| 模型强制 | `ANTHROPIC_MODEL` 强制 `deepseek-v4-flash`（.env 覆盖无效） | 事故根因：MBP pro ¥174 |
| 日配额 | 默认 50 次/天，超限拒绝 + 落 ledger | 无上限失控 |
| 单会话互斥 | topic 粒度 `.lock-*.json` 原子创建 | 并发 delegate 双回复 |
| 退避重试 | 2s→8s→30s 三次封顶 → dead-letter | 瞬时失败丢消息 |
| 触发收紧 | 仅 `llm:true` 显式标记触发 | v1 的 ask-/llm- 前缀宽触发 |
| 成本审计 | `llm-ledger.jsonl` 每次调用前后记录 | 成本有据可查 |

### 2. 防循环双保险（事故机制修复）
- **notes_loop**：LLM 消息若已在 `.processed`（处理过）→ 不再重写 inbox
  （v1.1.1 的 5s 写回 → llm_loop 再处理 → 无限重复的机制已堵死）
- **llm_loop**：非 LLM 消息不移动（保留 inbox 供会话读），不再 rename 到 .processed
- **should_trigger**：llm-reply- 前缀 + llm_reply:true 双标记绝不触发

### 3. 启用方式
- 默认启用（去掉 `--no-llm`）；想禁用仍可 `--no-llm`（v1.1.1 安全兜底保留）
- 启动日志验证：`LLM_EXEC v2 start node=xxx model=deepseek-v4-flash quota=50/day`

## 验证
- 24 tests passed（含 3 个新门禁测试：收紧触发 / 配额计数 / 互斥锁）
- 三平台编译通过（macOS arm64 / Windows x86_64 / Linux musl）

## 产物
- `dist/node-bridge-macos-arm64-v1.2.0`
- `dist/node-bridge-win-x64-v1.2.0.exe`
- `dist/node-bridge-linux-x64-v1.2.0`

## 回退
- 任何问题 → 重启服务带 `--no-llm`（回到禁用态，v1.1.1 行为）

## 追加修复（E2E 实测发现，已并入 v1.2.0 产物）

### 4. 黑板 notes 全量返回 bug（严重，直接影响 LLM 执行器）
- **现象**：`GET /notes/<node>/` 实际返回**全量 8777 条**（黑板不按 node 过滤！）
- **影响**：① 所有节点消息落盘本节点 inbox（污染）② LLM 执行器会**误处理其他节点的历史 llm:true 消息**（8/26 事故隐藏机制之一，每次启用都会处理 8/26 的 ask-auto-hello 等历史消息，意外烧 token）
- **修复**：notes_loop 只落盘 `notes/{node}/*`（自己）+ `notes/collab/*`（协作通道）
- **验证**：隔离 E2E 中 MSG 日志只含 mbp-e2e 与 collab 消息，其他节点消息不再进入

### 5. 防循环双保险（与 8/26 循环事故机制对齐）
- **notes_loop**：LLM 消息若已在 `.processed`（处理过）→ 不再重写 inbox（堵死「5s 写回 → llm_loop 再处理」无限循环）
- **llm_loop**：非 LLM 消息不移动（保留 inbox 供会话读），不再 rename 到 .processed

### 6. E2E 实测记录（8/27 00:19）
- 模拟：i9 发 `{"from":"i9","to":"mbp","llm":true,"content":"..."}` 到 inbox
- 结果：15s 内自动处理 → ledger 记账（call+ok）→ 回复写 outbox → 按 to 字段路由到 `notes/i9/llm-reply-hello-test`（from=mbp-e2e, llm_reply:true）✅
- 配额/审计/互斥/防循环全部生效
- 注：测试环境回复 "Not logged in" 是 HOME 隔离无 claude 登录态的假象，生产节点不受影响
