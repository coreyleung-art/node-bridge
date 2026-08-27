// exec.rs — 命令执行（跨平台 shell、超时、编码归一）
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// 执行 shell 命令，返回 (ok, output)。输出统一转 UTF-8（Windows GBK → UTF-8）。
pub fn run_shell(cmd: &str, timeout_secs: u64) -> (bool, String) {
    // Windows 用 cmd /C，其他用 sh -c
    let (program, arg) = if cfg!(windows) { ("cmd", "/C") } else { ("sh", "-c") };

    let mut child = match Command::new(program)
        .arg(arg)
        .arg(cmd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => return (false, format!("spawn error: {}", e)),
    };

    // 后台读 stdout/stderr（避免管道阻塞）
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut o) = out {
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut e) = err {
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });

    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) => {
                if start.elapsed() >= Duration::from_secs(timeout_secs) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return (false, "__TIMEOUT__".to_string());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = out_handle.join();
                let _ = err_handle.join();
                return (false, format!("wait error: {}", e));
            }
        }
    };

    let out_buf = out_handle.join().unwrap_or_default();
    let err_buf = err_handle.join().unwrap_or_default();
    let mut all = Vec::new();
    all.extend_from_slice(&out_buf);
    if !err_buf.is_empty() {
        if !all.is_empty() {
            all.push(b'\n');
        }
        all.extend_from_slice(&err_buf);
    }

    let output = decode_utf8(&all);
    (status.success(), output.trim().to_string())
}

/// 字节 → UTF-8 字符串：优先 UTF-8，失败按 GBK 解码（Windows 中文环境）
fn decode_utf8(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => {
            let (cow, _, _) = encoding_rs::GBK.decode(bytes);
            cow.into_owned()
        }
    }
}

/// info：节点环境信息（对齐 node-executor env_report）
pub fn info() -> (bool, String) {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    let v = serde_json::json!({
        "os": os,
        "encoding": if cfg!(windows) { "gbk" } else { "utf-8" },
        "bridge": "rust-node-bridge",
        "bridge_ver": env!("CARGO_PKG_VERSION"),
        "path_style": if cfg!(windows) { "\\" } else { "/" },
    });
    (true, v.to_string())
}

// ── 单元测试 ──
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_utf8_plain() {
        assert_eq!(decode_utf8(b"hello"), "hello");
    }

    #[test]
    fn decode_utf8_chinese_utf8() {
        assert_eq!(decode_utf8("中文测试".as_bytes()), "中文测试");
    }

    #[test]
    fn decode_utf8_gbk_fallback() {
        // GBK 编码的「中文」两字（0xD6D0 0xCEC4）——UTF-8 解析失败应回退 GBK
        let gbk = [0xD6, 0xD0, 0xCE, 0xC4];
        let s = decode_utf8(&gbk);
        assert_eq!(s, "中文", "GBK 字节应回退解码为中文");
    }

    #[test]
    fn decode_utf8_invalid_fallback_no_panic() {
        // 完全无效字节：不应 panic（GBK 解码也会产生替换字符）
        let bad = [0xFF, 0xFE, 0xFD];
        let s = decode_utf8(&bad);
        assert!(!s.is_empty() || s.is_empty()); // 不 panic 即通过
    }

    #[test]
    fn run_shell_echo() {
        let (ok, out) = run_shell("echo RUST_TEST_OK", 10);
        assert!(ok, "echo 应成功");
        assert!(out.contains("RUST_TEST_OK"), "输出应含标记: {}", out);
    }

    #[test]
    fn run_shell_timeout() {
        let (ok, out) = run_shell("sleep 30", 1);
        assert!(!ok, "超时应失败");
        assert!(out.contains("TIMEOUT"), "超时应返回 __TIMEOUT__: {}", out);
    }

    #[test]
    fn run_shell_nonzero_exit() {
        let (ok, out) = run_shell("exit 3", 5);
        assert!(!ok, "非零退出应失败");
        assert!(!out.contains("__TIMEOUT__"));
    }

    #[test]
    fn info_returns_json() {
        let (ok, out) = info();
        assert!(ok);
        let v: serde_json::Value = serde_json::from_str(&out).expect("info 输出应为 JSON");
        assert_eq!(v["bridge"], "rust-node-bridge");
        assert!(v["os"].is_string());
    }
}
