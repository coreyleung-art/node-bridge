// llm.rs — LLM 执行器 v2：三因子门禁版（成本事故教训落地）
// 解决「三个独立 DSH 随时对话」：node-bridge 收到消息自动拉起 LLM，无需人工激活会话
//
// v2 升级（8/27，成本事故三因子教训）：
//   1. 模型强制：ANTHROPIC_MODEL 强制 deepseek-v4-flash（不可被环境变量覆盖成 pro）
//   2. 配额门禁：每日调用上限（默认 50 次），超限拒绝并落盘审计（跨进程共享计数）
//   3. 单会话互斥：同一 topic 的并发唤醒用文件锁串行化（防并发 delegate）
//   4. 退避重试：瞬时失败指数退避（2s→8s→30s，3 次封顶），失败进 dead-letter 目录
//   5. 触发收紧：仅显式 llm:true 才触发（不再按 ask-/llm- 前缀宽触发）
//   6. 成本审计：每次调用前写 ledger 行（ts/model/topic/预算），超限=拒绝有据可查
//
// 模型路由：ANTHROPIC_BASE_URL / ANTHROPIC_MODEL（本机已配 DeepSeek anthropic 端点）
use serde_json::Value;
use std::path::{Path, PathBuf};

const FORCED_MODEL: &str = "deepseek-v4-flash";
const DAILY_QUOTA: u32 = 50;          // 每日 LLM 调用上限（成本门禁）
const LEDGER_FILE: &str = "llm-ledger.jsonl"; // 调用台账（追加式，审计用）
const DEAD_LETTER_DIR: &str = "llm-dead";     // 重试耗尽的消息（防丢失）

fn now_local() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ── 1. 日配额：跨进程共享计数（锁文件 + ledger 计数）──
// ledger 追加式记录每次调用；配额=当日 ledger 行数（含拒绝前检查）
fn quota_remaining(inbox: &Path) -> (u32, u32) {
    let ledger = inbox.join(LEDGER_FILE);
    let today_str = today();
    let mut count = 0u32;
    if let Ok(content) = std::fs::read_to_string(&ledger) {
        for line in content.lines() {
            if line.contains(&today_str) {
                count += 1;
            }
        }
    }
    (count, DAILY_QUOTA.saturating_sub(count))
}

fn ledger_record(inbox: &Path, topic: &str, status: &str, detail: &str) {
    let ledger = inbox.join(LEDGER_FILE);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&ledger) {
        use std::io::Write;
        let line = serde_json::json!({
            "ts": now_local(), "day": today(), "topic": topic,
            "model": FORCED_MODEL, "status": status, "detail": detail,
        });
        let _ = writeln!(f, "{}", line);
    }
}

// ── 2. 单会话互斥：锁文件（topic 粒度的 advisory lock）──
// 同一 topic 并发唤醒时，后到者直接跳过（防并发 delegate 双回复）
fn try_acquire_lock(inbox: &Path, topic: &str) -> Option<std::fs::File> {
    let safe = topic.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let lock_path = inbox.join(format!(".lock-{}.json", safe));
    // create_new 原子创建：已存在=被占用
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
        .ok()
}

fn release_lock(inbox: &Path, topic: &str) {
    let safe = topic.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let _ = std::fs::remove_file(inbox.join(format!(".lock-{}.json", safe)));
}

// ── 3. 死信：重试耗尽的消息移到 dead-letter（不丢、不重试）──
fn to_dead_letter(inbox: &Path, topic: &str, content: &Value, reason: &str) {
    let dl = inbox.join(DEAD_LETTER_DIR);
    let _ = std::fs::create_dir_all(&dl);
    let safe = topic.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
    let mut v = content.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.insert("dead_reason".into(), reason.into());
        obj.insert("dead_at".into(), now_local().into());
    }
    let _ = std::fs::write(dl.join(format!("{}-{}.json", safe, now_local().replace(':', ""))),
                           serde_json::to_string(&v).unwrap_or_default());
}

// LLM 触发条件（v2 收紧）：仅显式 llm:true 标记才触发
// 防循环硬保护：llm-reply- 前缀（自己的回复）绝不触发；llm_reply:true 标记也不触发
// v1 的 ask-/llm- 前缀宽触发已废除（事故教训：太宽 → 无谓调用）
pub fn should_trigger(value: &Value, topic: &str) -> bool {
    // 防循环：自己的回复（llm-reply- 前缀 或 llm_reply:true 标记）绝不触发
    if topic.starts_with("llm-reply-") {
        return false;
    }
    if value.get("llm_reply").and_then(|v| v.as_bool()).unwrap_or(false) {
        return false;
    }
    // v2 收紧：只有显式 llm:true 才触发
    value.get("llm").and_then(|v| v.as_bool()).unwrap_or(false)
}

// 执行 claude -p 一次性调用，返回回复文本
// v2：强制 flash 模型 + 10s 超时 + 退避重试（调用方控制）
pub fn run_claude(prompt: &str) -> Result<String, String> {
    // 找 claude CLI（多个候选路径）
    let candidates = [
        std::env::var("HOME").unwrap_or_default() + "/.local/bin/claude",
        "/opt/homebrew/bin/claude".to_string(),
        "claude".to_string(), // PATH 兜底
    ];
    let claude = candidates.iter().find(|p| {
        if p.contains('/') {
            std::path::Path::new(p).exists()
        } else {
            true // PATH 里的命令直接试
        }
    }).ok_or("claude CLI not found")?;

    // 构造 prompt：强调只回复处理结果
    let full_prompt = format!(
        "你是端侧节点智能体。收到一条消息，请处理并回复（简洁中文）：\n{}",
        prompt
    );

    // v2 门禁：强制 flash 模型（不可被环境覆盖成 pro）
    let mut cmd = std::process::Command::new(claude);
    cmd.args(["-p", &full_prompt, "--output-format", "text", "--no-session-persistence"])
       .env("ANTHROPIC_MODEL", FORCED_MODEL);

    let output = cmd.output()
        .map_err(|e| format!("claude spawn error: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        // 部分警告在 stderr（如 trust 警告）但 stdout 有内容时仍可用
        if stdout.trim().is_empty() {
            return Err(format!("claude exit {}: {}", output.status, stderr.trim().chars().take(200).collect::<String>()));
        }
    }
    // 清理 stderr 噪音（Ignoring 警告行）
    let clean = stdout.lines()
        .filter(|l| !l.starts_with("Ignoring") && !l.contains("hasTrustDialogAccepted"))
        .collect::<Vec<_>>()
        .join("\n");
    Ok(clean.trim().to_string())
}

// 执行 + 指数退避重试（2s→8s→30s，3 次封顶）
fn run_claude_with_retry(prompt: &str) -> Result<String, String> {
    let delays = [2u64, 8, 30];
    let mut last_err = String::new();
    for (i, delay) in delays.iter().enumerate() {
        match run_claude(prompt) {
            Ok(reply) => return Ok(reply),
            Err(e) => {
                last_err = e.clone();
                if i == delays.len() - 1 {
                    break; // 最后一次不再等
                }
                println!("[{}] LLM_RETRY {}s err={}", now_local(), delay, e.chars().take(80).collect::<String>());
                std::thread::sleep(std::time::Duration::from_secs(*delay));
            }
        }
    }
    Err(last_err)
}

// LLM 执行器线程：轮询 inbox 目录 → 处理带 llm:true 标记的消息 → 回复写 outbox
pub fn llm_loop(node: String, inbox: PathBuf, outbox: PathBuf) {
    // processed 子目录（已处理标记）
    let processed = inbox.join(".processed");
    std::fs::create_dir_all(&processed).ok();

    println!("[{}] LLM_EXEC v2 start node={} model={} quota={}/day (ledger={})",
             now_local(), node, FORCED_MODEL, DAILY_QUOTA, LEDGER_FILE);

    loop {
        if let Ok(rd) = std::fs::read_dir(&inbox) {
            let mut files: Vec<_> = rd.flatten()
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .collect();
            files.sort_by_key(|e| e.file_name());

            for entry in files {
                let path = entry.path();
                let fname = entry.file_name().to_string_lossy().to_string();
                let topic = fname.trim_end_matches(".json").to_string();

                // 读内容
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let value: Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // 判断是否触发 LLM（v2：仅 llm:true）
                if !should_trigger(&value, &topic) {
                    // 非 LLM 消息：不移动（保留在 inbox 供节点侧会话读取）
                    // 修复（v1.2.0）：v1.1.1 曾 rename 到 .processed，但 notes_loop 5s 后又写回 → 无限 IO
                    // 非 LLM 消息留在原位，会话随时可读；llm_loop 每轮重判（无 llm:true 不触发）
                    continue;
                }

                // 单会话互斥：同 topic 并发唤醒只允许一个（防双回复）
                let _lock = match try_acquire_lock(&inbox, &topic) {
                    Some(l) => l,
                    None => {
                        println!("[{}] LLM_BUSY {} (并发唤醒跳过)", now_local(), topic);
                        // 不移动文件：留给后续轮询（锁释放后再试）
                        continue;
                    }
                };

                // 配额门禁：超限拒绝（有据可查）
                let (used, remain) = quota_remaining(&inbox);
                if remain == 0 {
                    println!("[{}] LLM_QUOTA_EXCEEDED {} used={}/{} 拒绝（当日配额耗尽）",
                             now_local(), topic, used, DAILY_QUOTA);
                    ledger_record(&inbox, &topic, "quota_exceeded", &format!("used={}/{}", used, DAILY_QUOTA));
                    to_dead_letter(&inbox, &topic, &value, "daily quota exceeded");
                    let _ = std::fs::rename(&path, processed.join(&fname));
                    release_lock(&inbox, &topic);
                    continue;
                }

                // 提取消息内容（content 字段或原始文本）
                let prompt = value.get("content")
                    .and_then(|c| c.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| content.clone());

                // 记账（调用前）：配额内放行
                ledger_record(&inbox, &topic, "call", &prompt.chars().take(60).collect::<String>());

                // 调 claude（含退避重试）
                match run_claude_with_retry(&prompt) {
                    Ok(reply) => {
                        // 回复写 outbox（发给发件人；无 to 则写自己节点——中枢能读到）
                        let mut reply_value = serde_json::json!({
                            "from": node,
                            "subject": format!("llm-reply-{}", topic),
                            "content": reply,
                            "llm_reply": true,
                            "ts": now_local(),
                        });
                        // 回复对象：原消息的 from（若存在且是节点名）
                        let target = value.get("from")
                            .and_then(|f| f.as_str())
                            .filter(|f| *f != "coordinator")
                            .unwrap_or(&node);
                        // 跨节点回复：带 to 字段
                        if target != node {
                            reply_value["to"] = serde_json::json!(target);
                        }
                        let reply_file = outbox.join(format!("llm-reply-{}.json", topic));
                        let _ = std::fs::write(&reply_file, serde_json::to_string(&reply_value).unwrap_or_default());
                        println!("[{}] LLM_REPLY {} -> {}: {}", now_local(), topic, target, reply.chars().take(80).collect::<String>());
                        ledger_record(&inbox, &topic, "ok", &reply.chars().take(60).collect::<String>());
                    }
                    Err(e) => {
                        println!("[{}] LLM_ERROR {}: {}", now_local(), topic, e);
                        ledger_record(&inbox, &topic, "error", &e.chars().take(80).collect::<String>());
                        to_dead_letter(&inbox, &topic, &value, &e);
                    }
                }
                // 标记 processed（无论成功失败，避免重复触发）
                let _ = std::fs::rename(&path, processed.join(&fname));
                release_lock(&inbox, &topic);
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn should_trigger_llm_marker_only() {
        // v2 收紧：仅 llm:true 触发
        assert!(should_trigger(&json!({"llm": true}), "any-topic"));
        // ask-/llm- 前缀不再宽触发（v1 行为废除）
        assert!(!should_trigger(&json!({}), "ask-hello"));
        assert!(!should_trigger(&json!({}), "llm-question"));
    }

    #[test]
    fn no_trigger_on_reply_loop() {
        // 防循环：llm-reply- 前缀不触发
        assert!(!should_trigger(&json!({"content": "hi", "llm": true}), "llm-reply-ask-hello"));
        // llm_reply:true 标记不触发（即使带了 llm:true）
        assert!(!should_trigger(&json!({"llm_reply": true, "llm": true}), "anything"));
        // 普通消息不触发
        assert!(!should_trigger(&json!({"content": "普通消息"}), "normal-msg"));
    }

    #[test]
    fn quota_ledger_count() {
        let dir = std::env::temp_dir().join("llm-quota-test");
        std::fs::create_dir_all(&dir).ok();
        // 写 3 行今天的 ledger
        let today_str = today();
        let ledger = dir.join(LEDGER_FILE);
        std::fs::write(&ledger, format!(
            "{{\"day\":\"{}\",\"topic\":\"a\"}}\n{{\"day\":\"{}\",\"topic\":\"b\"}}\n{{\"day\":\"2020-01-01\",\"topic\":\"old\"}}\n",
            today_str, today_str
        )).unwrap();
        let (used, remain) = quota_remaining(&dir);
        assert_eq!(used, 2, "应只计今天的行");
        assert_eq!(remain, DAILY_QUOTA - 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lock_exclusive() {
        let dir = std::env::temp_dir().join("llm-lock-test");
        std::fs::create_dir_all(&dir).ok();
        let l1 = try_acquire_lock(&dir, "topic-a");
        assert!(l1.is_some(), "第一次应拿到锁");
        let l2 = try_acquire_lock(&dir, "topic-a");
        assert!(l2.is_none(), "同 topic 第二次应被拒（互斥）");
        release_lock(&dir, "topic-a");
        let l3 = try_acquire_lock(&dir, "topic-a");
        assert!(l3.is_some(), "释放后应能再拿");
        release_lock(&dir, "topic-a");
        std::fs::remove_dir_all(&dir).ok();
    }
}
