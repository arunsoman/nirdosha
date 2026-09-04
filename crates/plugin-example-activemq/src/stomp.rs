//! A minimal, hand-rolled STOMP 1.2 client over `std::net::TcpStream` --
//! see this crate's `README.md` for why this isn't built on an existing
//! STOMP crate. Just enough of the protocol for `CONNECT`/`SEND`/
//! `SUBSCRIBE`/`UNSUBSCRIBE`/`DISCONNECT` and reading back a `MESSAGE`
//! frame; no heart-beats, no reconnect logic, no receipt tracking --
//! this is a reference plugin proving the mechanism, not a
//! production-grade STOMP client.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

pub struct StompConn {
    stream: TcpStream,
    next_sub_id: u64,
}

/// Parses `stomp://[user[:pass]@]host:port` (the `stomp://` prefix is
/// optional) into `(login, passcode, host, port)`.
pub fn parse_url(url: &str) -> Result<(Option<String>, Option<String>, String, u16), String> {
    let rest = url.strip_prefix("stomp://").unwrap_or(url);
    let (auth, hostport) = match rest.rsplit_once('@') {
        Some((a, h)) => (Some(a), h),
        None => (None, rest),
    };
    let (login, passcode) = match auth {
        Some(a) => match a.split_once(':') {
            Some((u, p)) => (Some(u.to_string()), Some(p.to_string())),
            None => (Some(a.to_string()), None),
        },
        None => (None, None),
    };
    let (host, port) = hostport.rsplit_once(':').ok_or_else(|| format!("expected host:port in `{url}`"))?;
    let port: u16 = port.parse().map_err(|_| format!("invalid port in `{url}`"))?;
    Ok((login, passcode, host.to_string(), port))
}

impl StompConn {
    pub fn connect(host: &str, port: u16, login: Option<&str>, passcode: Option<&str>) -> Result<Self, String> {
        let stream = TcpStream::connect((host, port)).map_err(|e| format!("TCP connect failed: {e}"))?;
        stream.set_nodelay(true).ok();
        let mut conn = StompConn { stream, next_sub_id: 0 };

        let mut headers = vec!["accept-version:1.2".to_string(), format!("host:{host}")];
        if let Some(l) = login {
            headers.push(format!("login:{l}"));
        }
        if let Some(p) = passcode {
            headers.push(format!("passcode:{p}"));
        }
        conn.write_frame("CONNECT", &headers, "").map_err(|e| format!("write CONNECT failed: {e}"))?;
        let (command, _headers, body) =
            conn.read_frame(Some(Duration::from_secs(10))).map_err(|e| format!("STOMP handshake failed: {e}"))?;
        if command != "CONNECTED" {
            return Err(format!("STOMP server rejected CONNECT: {command} {body}"));
        }
        Ok(conn)
    }

    pub fn send(&mut self, destination: &str, body: &str) -> Result<(), String> {
        let headers = vec![format!("destination:{destination}"), "content-type:text/plain".to_string()];
        self.write_frame("SEND", &headers, body).map_err(|e| format!("SEND failed: {e}"))
    }

    /// Subscribes to `destination`, waits up to `timeout` for exactly
    /// one `MESSAGE` frame, unsubscribes, and returns the message body
    /// (`None` on timeout -- not an error; "no message arrived in time"
    /// is an ordinary, expected outcome for a polling consumer).
    pub fn receive_one(&mut self, destination: &str, timeout: Duration) -> Result<Option<String>, String> {
        let sub_id = self.next_sub_id;
        self.next_sub_id += 1;
        let id_header = format!("id:{sub_id}");
        let headers = vec![id_header.clone(), format!("destination:{destination}"), "ack:auto".to_string()];
        self.write_frame("SUBSCRIBE", &headers, "").map_err(|e| format!("SUBSCRIBE failed: {e}"))?;

        let deadline = std::time::Instant::now() + timeout;
        let result = loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break Ok(None);
            }
            match self.read_frame(Some(remaining)) {
                Ok((command, _headers, body)) if command == "MESSAGE" => break Ok(Some(body)),
                // Any other frame (e.g. a RECEIPT/ERROR) while waiting --
                // keep waiting for the actual MESSAGE, up to the deadline.
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                    break Ok(None);
                }
                Err(e) => break Err(format!("read failed while waiting for a message: {e}")),
            }
        };

        let unsub = vec![id_header];
        // Best-effort: an UNSUBSCRIBE write failing doesn't change what
        // we already received (or didn't) above.
        let _ = self.write_frame("UNSUBSCRIBE", &unsub, "");
        result
    }

    pub fn disconnect(&mut self) {
        let _ = self.write_frame("DISCONNECT", &[], "");
    }

    fn write_frame(&mut self, command: &str, headers: &[String], body: &str) -> io::Result<()> {
        let mut buf = String::new();
        buf.push_str(command);
        buf.push('\n');
        for h in headers {
            buf.push_str(h);
            buf.push('\n');
        }
        if !body.is_empty() {
            buf.push_str(&format!("content-length:{}\n", body.as_bytes().len()));
        }
        buf.push('\n');
        buf.push_str(body);
        buf.push('\0');
        self.stream.write_all(buf.as_bytes())
    }

    /// Reads one frame (command line, `key:value` headers, blank line,
    /// body, trailing NUL). Byte-at-a-time -- deliberately simple, not
    /// fast; fine for a reference plugin's request volume. Leading bare
    /// `\n`s before a frame starts (STOMP heart-beats) are skipped.
    fn read_frame(&mut self, timeout: Option<Duration>) -> io::Result<(String, Vec<(String, String)>, String)> {
        self.stream.set_read_timeout(timeout)?;
        let mut buf: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            match self.stream.read(&mut byte) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "connection closed by server")),
                Ok(_) => {
                    if byte[0] == 0 {
                        break;
                    }
                    if buf.is_empty() && byte[0] == b'\n' {
                        continue; // heart-beat newline before any real frame
                    }
                    buf.push(byte[0]);
                }
                Err(e) => return Err(e),
            }
        }
        let text = String::from_utf8_lossy(&buf).into_owned();
        let mut parts = text.splitn(2, "\n\n");
        let head = parts.next().unwrap_or("");
        let body = parts.next().unwrap_or("").trim_end_matches('\n').to_string();
        let mut lines = head.lines();
        let command = lines.next().unwrap_or("").to_string();
        let headers = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
        Ok((command, headers, body))
    }
}
