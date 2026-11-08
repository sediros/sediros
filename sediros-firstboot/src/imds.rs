//! EC2-style instance metadata service at 169.254.169.254 (AWS, Vultr,
//! Hetzner, OpenStack, ...). Plain HTTP on the link-local address, plain-text
//! answers, so a few lines over a TcpStream do instead of an HTTP library.

use crate::meta::{self, Metadata};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

pub const DEFAULT_ADDR: &str = "169.254.169.254:80";
const MAX_BODY: usize = 256 * 1024;

pub struct Imds {
    addr: SocketAddr,
    token: Option<String>,
}

/// (status, body)
fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> std::io::Result<(u16, String)> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    s.set_write_timeout(Some(Duration::from_secs(5)))?;
    // HTTP/1.0: no chunked encoding, the server closes after the answer.
    let mut req = format!("{method} {path} HTTP/1.0\r\nHost: 169.254.169.254\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    let mut raw = Vec::new();
    s.take(MAX_BODY as u64 + 8192).read_to_end(&mut raw)?;
    parse_response(&raw).ok_or_else(|| std::io::Error::other("malformed HTTP response"))
}

fn parse_response(raw: &[u8]) -> Option<(u16, String)> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n")?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, body.to_string()))
}

impl Imds {
    /// Waits up to `wait` for the service (the network may still be coming
    /// up); None when nothing answers, i.e. not on such a cloud.
    pub fn connect(addr: &str, wait: Duration) -> Option<Imds> {
        let addr: SocketAddr = addr.parse().ok()?;
        let start = Instant::now();
        loop {
            // IMDSv2 token (AWS may require it); others answer 404/405.
            let token_req = request(
                addr,
                "PUT",
                "/latest/api/token",
                &[("X-aws-ec2-metadata-token-ttl-seconds", "300")],
            );
            match token_req {
                Ok((200, token)) => {
                    return Some(Imds {
                        addr,
                        token: Some(token.trim().to_string()),
                    })
                }
                Ok(_) => return Some(Imds { addr, token: None }),
                Err(_) if start.elapsed() < wait => std::thread::sleep(Duration::from_secs(1)),
                Err(_) => return None,
            }
        }
    }

    fn get(&self, path: &str) -> Option<String> {
        let headers: Vec<(&str, &str)> = self
            .token
            .as_deref()
            .map(|t| vec![("X-aws-ec2-metadata-token", t)])
            .unwrap_or_default();
        match request(self.addr, "GET", path, &headers) {
            Ok((200, body)) => Some(body),
            _ => None,
        }
    }

    pub fn metadata(&self) -> Option<Metadata> {
        let mut md = Metadata {
            source: "instance metadata service",
            ..Default::default()
        };
        md.instance_id = self
            .get("/latest/meta-data/instance-id")?
            .trim()
            .to_string();
        if md.instance_id.is_empty() {
            return None;
        }
        md.hostname = self
            .get("/latest/meta-data/local-hostname")
            .or_else(|| self.get("/latest/meta-data/hostname"))
            .and_then(|h| meta::clean_hostname(&h));
        if let Some(list) = self.get("/latest/meta-data/public-keys/") {
            for i in meta::key_indexes(&list) {
                if let Some(keys) =
                    self.get(&format!("/latest/meta-data/public-keys/{i}/openssh-key"))
                {
                    md.ssh_keys.extend(keys.lines().filter_map(meta::clean_key));
                }
            }
        }
        if let Some(ud) = self.get("/latest/user-data") {
            meta::parse_user_data(&ud, &mut md);
        }
        Some(md)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn response_parsing() {
        assert_eq!(
            parse_response(b"HTTP/1.1 200 OK\r\nA: b\r\n\r\ni-123"),
            Some((200, "i-123".into()))
        );
        assert_eq!(
            parse_response(b"HTTP/1.0 404 Not Found\r\n\r\n"),
            Some((404, String::new()))
        );
        assert_eq!(parse_response(b"garbage"), None);
    }

    /// A fake metadata service: answers each request from `routes`.
    fn serve(routes: &'static [(&'static str, u16, &'static str)]) -> String {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in l.incoming().flatten() {
                let mut s = stream;
                let mut buf = [0u8; 4096];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let line = req.lines().next().unwrap_or("").to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(r, _, _)| line.starts_with(r))
                    .map(|(_, s, b)| (*s, *b))
                    .unwrap_or((404, ""));
                let _ = write!(
                    s,
                    "HTTP/1.0 {status} X\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        addr
    }

    #[test]
    fn reads_a_vultr_like_service() {
        let addr = serve(&[
            ("PUT /latest/api/token", 405, ""),
            ("GET /latest/meta-data/instance-id", 200, "a1b2c3\n"),
            ("GET /latest/meta-data/local-hostname", 200, "Web1.vultr.example"),
            ("GET /latest/meta-data/public-keys/ ", 200, "0=laptop\n"),
            ("GET /latest/meta-data/public-keys/0/openssh-key", 200, "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIKKaGH8rp2xWg2JosEbtDqoROBTW60PkhmpNVves42/U demo\n"),
            ("GET /latest/user-data", 200, "#!/bin/bash\necho hi\n"),
        ]);
        let imds = Imds::connect(&addr, Duration::from_secs(1)).expect("service");
        let md = imds.metadata().expect("metadata");
        assert_eq!(md.instance_id, "a1b2c3");
        assert_eq!(md.hostname.as_deref(), Some("web1"));
        assert_eq!(md.ssh_keys.len(), 1);
        assert_eq!(md.user_data, crate::meta::UserData::Script);
    }

    #[test]
    fn nothing_listening_means_no_cloud() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        drop(l);
        assert!(Imds::connect(&addr, Duration::from_millis(10)).is_none());
    }
}
