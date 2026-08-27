
## v1.3.0 (2026-08-28)
- **新增**: --identity 参数（P1-3b）——启动时读/生成 identity.json（device_id + token），注册节点带 device_id
- **新增**: --token 参数（P1-1c 预留）——黑板认证 token
- **修复**: 启动注册改 POST /register（黑板 v0.6.3）

## v1.3.1 (2026-08-28)
- **新增**: --token 真正生效（Bb::with_token + X-Blackboard-Token 头，P1-1c 逐步启用基础）
