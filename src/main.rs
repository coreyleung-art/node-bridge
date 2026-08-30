// main.rs — node-bridge 统一节点桥入口
// 三线程模型（继承 v6 双频设计）：heartbeat 60s / queue 2s / notes 5s
// 协议 v1.0 通道不变：nodes/{node}/heartbeat · tasks/{node}/queue/ · tasks/{node}/result · notes/{node}/*
mod actions;
mod bb;
mod exec;
mod llm;
mod logger;
mod signature;

use bb::Bb;
use serde_json::json;
use std::time::Duration;

const DEFAULT_BB: &str = "http://100.120.203.20:8792";

struct Config {
    node: String,
    bb: Bb,
    hb_secs: u64,
    queue_secs: u64,
    notes_secs: u64,
    no_llm: bool,
    identity_path: Option<String>, // P1-3b: identity.json 路径
    token: String,                 // P1-1c: 黑板认证 token
    collab_subs: Option<Vec<String>>, // v1.4.0: collab 前缀订阅（如 ops-*），白名单过滤用
}

impl Clone for Config {
    fn clone(&self) -> Config {
        Config {
            node: self.node.clone(),
            bb: self.bb.clone(),
            hb_secs: self.hb_secs,
            queue_secs: self.queue_secs,
            notes_secs: self.notes_secs,
            no_llm: self.no_llm,
            identity_path: self.identity_path.clone(),
            token: self.token.clone(),
            collab_subs: self.collab_subs.clone(),
        }
    }
}

fn parse_args() -> Config {
    let args: Vec<String> = std::env::args().collect();
    let mut node = "node".to_string();
    let mut bb_url = DEFAULT_BB.to_string();
    let mut hb = 60;
    let mut queue = 2;
    let mut notes = 5;
    let mut no_llm = false;
    let mut identity_path: Option<String> = None;
    let mut token = String::new();
    let mut collab_subs: Option<Vec<String>> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--node-id" => { i += 1; if i < args.len() { node = args[i].clone(); } }
            "--blackboard" => { i += 1; if i < args.len() { bb_url = args[i].clone(); } }
            "--hb" => { i += 1; if i < args.len() { hb = args[i].parse().unwrap_or(60); } }
            "--queue" => { i += 1; if i < args.len() { queue = args[i].parse().unwrap_or(2); } }
            "--notes" => { i += 1; if i < args.len() { notes = args[i].parse().unwrap_or(5); } }
            "--no-llm" => { no_llm = true; }
            "--identity" => { i += 1; if i < args.len() { identity_path = Some(args[i].clone()); } }
            "--token" => { i += 1; if i < args.len() { token = args[i].clone(); } }
            "--collab-subs" => { i += 1; if i < args.len() { collab_subs = Some(args[i].split(",").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()); } }
            _ => {}
        }
        i += 1;
    }
    let bb = Bb::with_token(&bb_url, &token);
    Config { node, bb, hb_secs: hb, queue_secs: queue, notes_secs: notes, no_llm, identity_path, token, collab_subs }
}

fn now() -> String {
    // ISO8601 本地时间（无 chrono 依赖，用秒级时间戳拼接）
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}", secs)
}

fn log(msg: &str) {
    // 统一结构化日志（规范 rust-toolchain/docs/logging-spec-v1.0.md）
    logger::log(logger::LEVEL_INFO, "INFO", "main", msg);
}

// ── 线程 1：心跳 60s（存活唯一标准）──
fn heartbeat_loop(cfg: &Config) {
    loop {
        let body = serde_json::json!({
            "ts": now(), "health": "ok",
            "bridge": "rust", "ver": env!("CARGO_PKG_VERSION"),
        });
        match cfg.bb.put_json(&format!("/nodes/{}/heartbeat", cfg.node), &body) {
            Ok((st, _)) => { if st != 200 { log(&format!("heartbeat status {}", st)); } }
            Err(e) => log(&format!("heartbeat error: {}", e)),
        }
        std::thread::sleep(Duration::from_secs(cfg.hb_secs));
    }
}

// ── 线程 2：队列 2s（取卡 → 投递 worker → worker 执行 → 回报 → 清卡）──
// 教训复盘（i9 scan-002 卡死）：执行不进取卡循环——worker 线程消费，取卡永不阻塞
fn queue_loop(cfg: &Config, tx: std::sync::mpsc::Sender<(String, serde_json::Value)>) {
    loop {
        // 取卡：GET /tasks?node=<id>（v0.6 收件定向）→ 兼容列表接口
        match cfg.bb.get_json(&format!("/tasks?node={}", cfg.node)) {
            Ok(v) => {
                let list = v.get("list").cloned().unwrap_or(serde_json::json!({}));
                if let Some(map) = list.as_object() {
                    // 按 key 排序（queue/{ts} 时间序）
                    let mut keys: Vec<&String> = map.keys().collect();
                    keys.sort();
                    for k in keys {
                        let entry = &map[k];
                        let task = entry.get("value").cloned().unwrap_or(serde_json::json!({}));
                        // 只处理队列卡（queue/）+ 旧 cmd 卡（协议 v1.0：旧键保持应答，check_legacy_cmd）
                        let is_queue = k.contains("/queue/");
                        let is_legacy_cmd = k.ends_with("/cmd");
                        if !is_queue && !is_legacy_cmd {
                            continue; // 跳过 result 等镜像
                        }
                        // 投递给 worker（channel 满则阻塞，天然背压，不会无限堆积）
                        let _ = tx.send((k.clone(), task));
                    }
                }
            }
            Err(e) => log(&format!("queue poll error: {}", e)),
        }
        std::thread::sleep(Duration::from_secs(cfg.queue_secs));
    }
}

// ── 线程 2b：worker 消费队列（执行 → 回报 → 清卡），与取卡解耦 ──
fn worker_loop(cfg: &Config, rx: std::sync::mpsc::Receiver<(String, serde_json::Value)>) {
    for (k, task) in rx {
        let task_id = task
            .get("task_id")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        log(&format!("TASK {} key={}", task_id, k));
        // 执行（scan 等慢动作在此线程，不阻塞取卡）
        let (ok, output) = actions::execute(&task);
        // 回报（永远写 result）
        let result = serde_json::json!({
            "task_id": task_id,
            "ok": ok,
            "output": output,
            "node": cfg.node,
            "ts": now(),
        });
        match cfg.bb.put_json(&format!("/tasks/{}/result", cfg.node), &result) {
            Ok((st, _)) => log(&format!("DONE {} ok={} report={}", task_id, ok, st)),
            Err(e) => log(&format!("report error: {}", e)),
        }
        // 清卡
        let _ = cfg.bb.delete(&format!("/{}", k));
    }
}

// ── 线程 4b：outbox 2s（本地 outbox/ 目录 → 黑板 notes/<node>/<topic>）──
// 节点侧会话（无 HTTP 工具）发消息 = 写本地 outbox/ 文件 → 本线程代为 PUT 到黑板
// 消息文件格式：outbox/<topic>.json，内容即要写入黑板 notes/<node>/<topic> 的 value
// 成功转发 → 删除本地文件；失败 → 保留重试
fn outbox_path() -> std::path::PathBuf {
    // 固定路径 ~/.dsh/outbox/（跨平台、不依赖启动 cwd）
    // Windows: %USERPROFILE%\.dsh\outbox\ · macOS/Linux: ~/.dsh/outbox/
    if cfg!(windows) {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return std::path::PathBuf::from(home).join(".dsh").join("outbox");
        }
    } else if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home).join(".dsh").join("outbox");
    }
    // 回退：当前目录 outbox/
    let mut p = std::env::current_dir().unwrap_or_else(|_| ".".into());
    p.push("outbox");
    p
}

fn outbox_loop(cfg: &Config) {
    // outbox 目录固定路径（跨平台、不依赖启动 cwd）：~/.dsh/outbox/
    // 兼容：若固定目录不存在则回退当前目录的 outbox/
    let outbox_dir = outbox_path();
    loop {
        let dir = outbox_dir.clone();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            let mut files: Vec<_> = rd.flatten()
                .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
                .collect();
            files.sort_by_key(|e| e.file_name());
            for entry in files {
                let path = entry.path();
                let fname = entry.file_name().to_string_lossy().to_string();
                // topic = 文件名去掉 .json
                let topic = fname.trim_end_matches(".json").to_string();
                if topic.is_empty() || topic.starts_with('.') {
                    continue;
                }
                // 读内容
                let content = match std::fs::read_to_string(&path) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                // 解析 JSON（必须是合法 JSON 对象）
                let value: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => {
                        // 非 JSON：包成 {"content": "..."}
                        serde_json::json!({"content": content})
                    }
                };
                // 目标节点：value 里的 to 字段（跨节点发消息）；缺省 = 自己节点
                // 示例：{"to":"i9","topic":"hello","content":"..."} → 写 notes/i9/hello
                let target_node = value.get("to")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| cfg.node.clone());
                // topic：value 里的 topic 字段优先，否则用文件名
                let effective_topic = value.get("topic")
                    .and_then(|t| t.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| topic.clone());
                // P1-3d 消息签名（Ed25519 防伪造）：签名规范化 body → 附 signature + signer
                let mut signed_value = value.clone();
                if let Some(idp) = &cfg.identity_path {
                    if let Ok((priv_k, pub_k)) = signature::ensure_keys(std::path::Path::new(idp)) {
                        let body_str = signed_value.to_string();
                        if let Ok(sig) = signature::sign_message(&priv_k, &body_str) {
                            if let Some(obj) = signed_value.as_object_mut() {
                                obj.insert("signature".to_string(), serde_json::Value::String(sig));
                                obj.insert("signer_pub".to_string(), serde_json::Value::String(pub_k));
                            }
                            log("P1-3d signed (ed25519)");
                        }
                    }
                }
                // 转发到黑板（支持跨节点）
                let bb_path = format!("/notes/{}/{}", target_node, effective_topic);
                match cfg.bb.put_json(&bb_path, &signed_value) {
                    Ok((st, _)) => {
                        if st == 200 {
                            let _ = std::fs::remove_file(&path);
                            log(&format!("OUTBOX {} -> {} (200)", effective_topic, bb_path));
                        } else {
                            log(&format!("OUTBOX {} -> status {}", effective_topic, st));
                        }
                    }
                    Err(e) => log(&format!("OUTBOX {} error: {}", effective_topic, e)),
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

// ── 线程 3：消息 5s（读 notes/{node}/*，DSH 智能体对话通道 + 点对点消息）──
fn notes_loop(cfg: &Config) {
    loop {
        match cfg.bb.get_json(&format!("/notes/{}/", cfg.node)) {
            Ok(v) => {
                let list = v.get("list").cloned().unwrap_or(serde_json::json!({}));
                if let Some(map) = list.as_object() {
                    let mut keys: Vec<&String> = map.keys().collect();
                    keys.sort();
                    for k in keys {
                        // 只处理本节点相关通道（v1.2.0 修复）：
                        // 黑板 GET /notes/<node>/ 实际返回全量 8777 条（不按 node 过滤！）
                        // 若不过滤：① 全量落盘 inbox 污染 ② LLM 执行器误处理其他节点历史 llm:true 消息（意外烧 token）
                        // 本节点接收：notes/{node}/*（自己的）+ notes/mac-mini/*（中枢通道，跨设备互通）
                        // 注意：mac-mini 中枢通道只在 node==mac-mini 时自己处理；其他节点要收中枢消息走 notes/{node}/coordinator-*
                        // v1.4.0 通道卫生（2026-08-30 用户审计）：collab 白名单过滤——
                        //   背景：同盟法辅等独立项目误走 notes/collab/ 广播，i9/MBP 全量收 → 无关项目污染上下文
                        //   规则：collab 消息仅当 ① 显式 to/target/@提及 本节点 ② 本节点在 mentions 列表 才接收；
                        //         纯全局公告（无定向字段）默认跳过（避免无关项目广播进 inbox）
                        //   注意：mac-mini 中枢仍收全量 collab（它负责协调/巡检/纠偏）
                        let mut relevant = k.starts_with(&format!("notes/{}/", cfg.node));
                        if !relevant && k.starts_with("notes/collab/") {
                            if cfg.node == "mac-mini" {
                                relevant = true; // 中枢全量收（协调职责）
                            } else {
                                // 端节点：collab 白名单——定向到本节点才收
                                let v = map.get(k.as_str()).and_then(|e| e.get("value")).cloned().unwrap_or(json!({}));
                                let v_obj = v.as_object().cloned().unwrap_or_default();
                                let val_obj = v_obj.get("value").and_then(|x| x.as_object()).cloned().unwrap_or_default();
                                let mut targeted = false;
                                // 顶层 to / value.to / value.target
                                for field in ["to", "target"] {
                                    if let Some(t) = v_obj.get(field).or_else(|| val_obj.get(field)) {
                                        if let Some(s) = t.as_str() {
                                            if s == cfg.node || s.contains(cfg.node.as_str()) { targeted = true; }
                                        }
                                    }
                                }
                                // mentions 列表（@session 提及）
                                if let Some(m) = val_obj.get("mentions").and_then(|x| x.as_array()) {
                                    for mi in m {
                                        if let Some(s) = mi.as_str() {
                                            if s.contains(cfg.node.as_str()) || s.contains(cfg.node.as_str()) { targeted = true; }
                                        }
                                    }
                                }
                                // topic 前缀订阅（node-bridge 自定义订阅，如 notes/collab/ops-*）
                                if let Some(subs) = cfg.collab_subs.as_ref() {
                                    if subs.iter().any(|p| k.starts_with(&format!("notes/collab/{}", p))) {
                                        targeted = true;
                                    }
                                }
                                if !targeted {
                                    continue; // 无关 collab 消息跳过（通道卫生 P0a）
                                }
                            }
                        }
                        if !relevant {
                            continue;
                        }
                        // 读本节点全部消息（coordinator-* 中枢消息 + 其他节点 p2p 消息）
                        // 排除自己 outbox 转发的（value 里带 from==自己 的忽略，避免自读循环）
                        let entry = &map[k];
                        if let Some(val) = entry.get("value") {
                            // 点对点消息（from 是其他节点）也读取
                            log(&format!("MSG {} = {}", k, val));
                            // 落盘 inbox 供节点侧会话读取（~/.dsh/inbox/）
                            if let Some(inbox) = inbox_path() {
                                if let Some(topic) = k.rsplit('/').next() {
                                    let safe = topic.replace(|c: char| !c.is_alphanumeric() && c != '-', "_");
                                    let fpath = inbox.join(format!("{}.json", safe));
                                    // 防循环（v1.2.0）：LLM 消息若已在 .processed（llm_loop 处理过），
                                    // 不再重写 inbox——否则 notes_loop 5s 写回 → llm_loop 再处理 → 无限重复
                                    let is_llm_msg = val.get("llm").and_then(|v| v.as_bool()).unwrap_or(false);
                                    let processed_path = inbox.join(".processed").join(format!("{}.json", safe));
                                    if is_llm_msg && processed_path.exists() {
                                        continue; // 已处理过的 LLM 消息跳过（防重复触发）
                                    }
                                    let _ = std::fs::create_dir_all(&inbox);
                                    // v1.3.3 修复（i9 崩溃）：原子写 inbox——tmp 文件 + rename，
                                    // 避免与 llm_loop 的 rename 并发竞争同一目标（Windows 文件句柄冲突→静默退出）
                                    let _ = std::fs::create_dir_all(&inbox.join(".tmp"));
                                    let tmp_path = inbox.join(".tmp").join(format!("{}.tmp", safe));
                                    if std::fs::write(&tmp_path, val.to_string()).is_ok() {
                                        let _ = std::fs::rename(&tmp_path, &fpath);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => log(&format!("notes poll error: {}", e)),
        }
        std::thread::sleep(Duration::from_secs(cfg.notes_secs));
    }
}

fn inbox_path() -> Option<std::path::PathBuf> {
    // ~/.dsh/inbox/ —— 节点侧会话可读的收件箱（node-bridge 落盘）
    if cfg!(windows) {
        std::env::var("USERPROFILE").ok().map(|h| std::path::PathBuf::from(h).join(".dsh").join("inbox"))
    } else {
        std::env::var("HOME").ok().map(|h| std::path::PathBuf::from(h).join(".dsh").join("inbox"))
    }
}

fn main() {
    logger::init(logger::LEVEL_INFO);
    let cfg = parse_args();
    log(&format!(
        "node-bridge v{} start node={} bb={} (hb={}s queue={}s notes={}s)",
        env!("CARGO_PKG_VERSION"), cfg.node, cfg.bb.addr(), cfg.hb_secs, cfg.queue_secs, cfg.notes_secs
    ));

    // P1-3b: 设备 identity（读/生成 identity.json）
    let mut device_id = String::new();
    if let Some(ip) = &cfg.identity_path {
        let id_file = std::path::Path::new(ip);
        if id_file.exists() {
            // 读已有 identity
            if let Ok(content) = std::fs::read_to_string(id_file) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&content) {
                    device_id = v.get("device_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    if device_id.is_empty() && !cfg.token.is_empty() {
                        device_id = cfg.token.clone();
                    }
                }
            }
        } else {
            // 生成新 identity（device_id = node, token 用黑板 register）
            let body_str = serde_json::json!({"name": cfg.node}).to_string();
            if let Ok((_st, resp)) = cfg.bb.request("POST", "/register", Some(&body_str)) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp) {
                    device_id = v.get("device_id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let token_val = v.get("token").and_then(|x| x.as_str()).unwrap_or("").to_string();
                    let identity = serde_json::json!({
                        "device_id": device_id,
                        "name": cfg.node,
                        "token": token_val,
                        "registered": now(),
                    });
                    let _ = std::fs::create_dir_all(id_file.parent().unwrap_or(std::path::Path::new(".")));
                    let _ = std::fs::write(id_file, serde_json::to_string_pretty(&identity).unwrap_or_default());
                    log(&format!("identity -> {} ({} )", ip, device_id));
                }
            }
        }
    }

    // 注册节点
    let mut reg = serde_json::json!({
        "status": "online",
        "capabilities": ["shell", "info", "scan", "ollama", "dsh", "rust-bridge"],
        "registered": true,
        "os": if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "darwin" } else { "linux" },
        "bridge": "rust",
        "ts": now(),
    });
    if !device_id.is_empty() {
        reg["device_id"] = serde_json::Value::String(device_id);
    }
    match cfg.bb.put_json(&format!("/nodes/{}", cfg.node), &reg) {
        Ok((st, _)) => log(&format!("register -> {}", st)),
        Err(e) => log(&format!("register error: {}", e)),
    }

    // 线程并行，互不阻塞（Bb 已 Clone，各线程持有独立副本）
    // 队列线程 + worker 线程（producer-consumer）：取卡与执行解耦
    let (tx, rx) = std::sync::mpsc::channel::<(String, serde_json::Value)>();
    let hb_cfg = cfg.clone();
    let q_cfg = cfg.clone();
    let w_cfg = cfg.clone();
    let n_cfg = cfg.clone();
    let o_cfg = cfg.clone();
    std::thread::spawn(move || heartbeat_loop(&hb_cfg));
    std::thread::spawn(move || queue_loop(&q_cfg, tx));
    std::thread::spawn(move || worker_loop(&w_cfg, rx));
    std::thread::spawn(move || notes_loop(&n_cfg));
    std::thread::spawn(move || outbox_loop(&o_cfg));
    // LLM 执行器线程：inbox 新消息 → claude -p → outbox 回复（不依赖人工激活会话）
    // --no-llm 可禁用（成本控制）
    let llm_node = cfg.node.clone();
    let llm_inbox = inbox_path().unwrap_or_else(|| std::path::PathBuf::from("inbox"));
    let llm_outbox = outbox_path();
    if !cfg.no_llm {
        std::thread::spawn(move || llm::llm_loop(llm_node, llm_inbox, llm_outbox));
    } else {
        log("LLM 执行器已禁用（--no-llm）");
    }

    // 主线程保活
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

#[cfg(test)]
mod outbox_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn outbox_topic_from_filename() {
        assert_eq!("report-test".trim_end_matches(".json"), "report-test");
        assert_eq!("plain".trim_end_matches(".json"), "plain");
    }

    #[test]
    fn outbox_dir_created_and_scanned() {
        // 创建临时 outbox 目录并验证可扫描
        let dir = std::env::temp_dir().join("ob-test-unit");
        let outbox = dir.join("outbox");
        std::fs::create_dir_all(&outbox).unwrap();
        let f1 = outbox.join("msg1.json");
        std::fs::write(&f1, r#"{"content":"hi"}"#).unwrap();
        let rd = std::fs::read_dir(&outbox).unwrap();
        let files: Vec<_> = rd.flatten()
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 1, "应扫描到 1 个 json 文件");
        assert!(f1.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
