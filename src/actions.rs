// actions.rs — 动作分派（协议 v1.0 白名单：shell/info/status/scan/ollama/dsh）
use crate::bb::Bb;
use crate::exec;

/// 执行任务卡动作，返回 (ok, output)
pub fn execute(task: &serde_json::Value) -> (bool, String) {
    let action = task
        .get("action")
        .and_then(|a| a.as_str())
        .unwrap_or("shell");
    let payload = task.get("payload").cloned().unwrap_or(serde_json::json!({}));
    let cmd = task
        .get("cmd")
        .and_then(|c| c.as_str())
        .or_else(|| payload.get("cmd").and_then(|c| c.as_str()))
        .unwrap_or("");

    match action {
        "shell" | "exec" | "run" => exec::run_shell(cmd, 300),
        "info" | "status" => exec::info(),
        "scan" => scan(&payload),
        "ollama" => ollama(&payload),
        "dsh" => (false, "dsh action requires DSH runtime on node; not yet wired".to_string()),
        other => (false, format!("unknown action: {}", other)),
    }
}

/// scan：扫描目录 → JSON 清单（对齐 i9 _scan）
fn scan(payload: &serde_json::Value) -> (bool, String) {
    let path = payload.get("path").and_then(|p| p.as_str()).unwrap_or(".");
    let depth = payload.get("depth").and_then(|d| d.as_u64()).unwrap_or(2) as usize;
    let mut items: Vec<serde_json::Value> = Vec::new();
    walk(std::path::Path::new(path), 0, depth, &mut items);
    let v = serde_json::json!({
        "root": path,
        "count": items.len(),
        "items": items,
    });
    (true, v.to_string())
}

fn walk(dir: &std::path::Path, level: usize, max_depth: usize, items: &mut Vec<serde_json::Value>) {
    if level > max_depth || items.len() > 2000 {
        return;
    }
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            walk(&p, level + 1, max_depth, items);
        } else {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            items.push(serde_json::json!({
                "path": p.to_string_lossy(),
                "size": size,
                "type": p.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default(),
            }));
        }
    }
}

/// ollama：调节点本地 Ollama（http://localhost:11434），零订阅
fn ollama(payload: &serde_json::Value) -> (bool, String) {
    let model = payload.get("model").and_then(|m| m.as_str()).unwrap_or("qwen2.5:7b");
    let prompt = payload.get("prompt").and_then(|p| p.as_str()).unwrap_or("");
    let body = serde_json::json!({
        "model": model, "prompt": prompt, "stream": false,
        "options": {"temperature": 0},
    });
    // 直接用 Bb 客户端打 localhost:11434
    let bb = Bb::new("http://localhost:11434");
    match bb.put_json("/api/generate", &body) {
        Ok((st, resp)) => {
            if st != 200 {
                return (false, format!("ollama http {}", st));
            }
            match serde_json::from_str::<serde_json::Value>(&resp) {
                Ok(v) => {
                    let text = v.get("response").and_then(|r| r.as_str()).unwrap_or("").to_string();
                    (true, text.trim().to_string())
                }
                Err(e) => (false, format!("ollama bad json: {}", e)),
            }
        }
        Err(e) => (false, format!("ollama error: {}", e)),
    }
}

// ── 单元测试 ──
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_action_returns_false_not_panic() {
        let task = serde_json::json!({"action": "notify", "cmd": "whatever"});
        let (ok, out) = execute(&task);
        assert!(!ok);
        assert!(out.contains("unknown action"), "应返回 unknown action 错误: {}", out);
    }

    #[test]
    fn shell_action_default() {
        let task = serde_json::json!({"action": "shell", "cmd": "echo ACTION_OK"});
        let (ok, out) = execute(&task);
        assert!(ok);
        assert!(out.contains("ACTION_OK"));
    }

    #[test]
    fn info_action() {
        let task = serde_json::json!({"action": "info"});
        let (ok, out) = execute(&task);
        assert!(ok);
        assert!(out.contains("rust-node-bridge"));
    }

    #[test]
    fn cmd_from_payload() {
        let task = serde_json::json!({"action": "shell", "payload": {"cmd": "echo PAYLOAD_OK"}});
        let (ok, out) = execute(&task);
        assert!(ok);
        assert!(out.contains("PAYLOAD_OK"));
    }

    #[test]
    fn scan_limits_depth() {
        let task = serde_json::json!({"action": "scan", "payload": {"path": "/tmp", "depth": 1}});
        let (ok, out) = execute(&task);
        assert!(ok);
        let v: serde_json::Value = serde_json::from_str(&out).expect("scan 输出应为 JSON");
        assert!(v["count"].is_number());
        assert!(v["root"] == "/tmp");
    }

    #[test]
    fn ollama_offline_graceful() {
        // 无本地 ollama 时应优雅返回错误，不 panic
        let task = serde_json::json!({"action": "ollama", "payload": {"prompt": "hi"}});
        let (ok, _) = execute(&task);
        assert!(!ok, "离线 ollama 应返回失败而非 panic");
    }
}
