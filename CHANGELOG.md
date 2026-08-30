## v1.3.3 (2026-08-30)
- **修复（i9 崩溃）**: notes_loop 落盘改原子写（tmp+rename）——避免与 llm_loop 的 rename 并发竞争同一 inbox 文件（Windows 文件句柄冲突→静默退出，i9 批量消费积压 notes 时触发）
## v1.3.2 (2026-08-29)
- **修复（P1-1c 阻塞级）**: bb.rs request() 请求头构造——token_hdr 自带 \r\n + 格式串再补一个 → header 出现空行提前结束，Content-Length 被吞进 body。带 token 时所有写入 value 变 {}（心跳/队列/notes 全空）、register 名字丢失（默认 device）。修复后 token 请求体完整送达
- **修复**: 启动日志打印 DEFAULT_BB 常量而非实际 bb 地址（误导排查）；新增 Bb::addr() 访问器

## v1.3.0 (2026-08-28)
- **新增**: --identity 参数（P1-3b）——启动时读/生成 identity.json（device_id + token），注册节点带 device_id
- **新增**: --token 参数（P1-1c 预留）——黑板认证 token
- **修复**: 启动注册改 POST /register（黑板 v0.6.3）

## v1.3.1 (2026-08-28)
- **新增**: --token 真正生效（Bb::with_token + X-Blackboard-Token 头，P1-1c 逐步启用基础）
