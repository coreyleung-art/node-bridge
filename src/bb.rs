// bb.rs — 黑板 HTTP 客户端（纯 std::net，无外部依赖，交叉编译零负担）
use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

#[derive(Clone)]
pub struct Bb {
    host: String,
    port: u16,
    token: String, // P1-1c: 黑板认证 token（可选，空=不带）
}

impl Bb {
    pub fn new(url: &str) -> Bb {
        Self::with_token(url, "")
    }

    /// 带 token 构造（P1-1c）
    pub fn with_token(url: &str, token: &str) -> Bb {
        let u = url.trim_start_matches("http://").trim_end_matches('/');
        let (host, port) = match u.rfind(':') {
            Some(i) => (u[..i].to_string(), u[i + 1..].parse().unwrap_or(8792)),
            None => (u.to_string(), 8792),
        };
        Bb { host, port, token: token.to_string() }
    }

    /// 发请求，返回 (status, body_text)。Connection: close 简单模型。
    pub fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
        let mut stream = TcpStream::connect((self.host.as_str(), self.port))
            .map_err(|e| format!("connect {}:{} -> {}", self.host, self.port, e))?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))
            .map_err(|e| format!("set timeout: {}", e))?;
        stream.set_write_timeout(Some(Duration::from_secs(15)))
            .map_err(|e| format!("set timeout: {}", e))?;

        let body = body.unwrap_or("");
        let token_hdr = if self.token.is_empty() {
            String::new()
        } else {
            format!("X-Blackboard-Token: {}\r\n", self.token)
        };
        let req = format!(
            "{} {} HTTP/1.1\r\nHost: {}:{}\r\n{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            method, path, self.host, self.port, token_hdr, body.len(), body
        );
        stream.write_all(req.as_bytes()).map_err(|e| format!("write: {}", e))?;

        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).map_err(|e| format!("read: {}", e))?;
        let text = String::from_utf8_lossy(&resp).to_string();
        // 解析状态码
        let status: u16 = text
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // 分离 header/body
        let body_part = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
        Ok((status, body_part))
    }

    pub fn get(&self, path: &str) -> Result<(u16, String), String> {
        self.request("GET", path, None)
    }

    pub fn put_json(&self, path: &str, v: &serde_json::Value) -> Result<(u16, String), String> {
        self.request("PUT", path, Some(&v.to_string()))
    }

    pub fn delete(&self, path: &str) -> Result<(u16, String), String> {
        self.request("DELETE", path, None)
    }

    /// 取 JSON，黑板约定：成功返回 {"key":..., "value":...} 或 {"list":...}
    pub fn get_json(&self, path: &str) -> Result<serde_json::Value, String> {
        let (st, body) = self.get(path)?;
        if st != 200 {
            return Err(format!("GET {} -> {}", path, st));
        }
        serde_json::from_str(&body).map_err(|e| format!("bad json {}: {}", path, e))
    }
}

// ── 单元测试 ──
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_url_with_port() {
        let b = Bb::new("http://100.120.203.20:8792");
        assert_eq!(b.host, "100.120.203.20");
        assert_eq!(b.port, 8792);
    }

    #[test]
    fn parse_url_no_port_default() {
        let b = Bb::new("http://10.0.0.1");
        assert_eq!(b.host, "10.0.0.1");
        assert_eq!(b.port, 8792);
    }

    #[test]
    fn parse_url_trailing_slash() {
        let b = Bb::new("http://host:9000/");
        assert_eq!(b.host, "host");
        assert_eq!(b.port, 9000);
    }

    #[test]
    fn parse_url_hostname() {
        let b = Bb::new("http://blackboard.local:1234");
        assert_eq!(b.host, "blackboard.local");
        assert_eq!(b.port, 1234);
    }
}
