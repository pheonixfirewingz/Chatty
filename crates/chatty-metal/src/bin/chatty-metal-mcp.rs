//! MCP server for debugging a chatty-metal test image on real hardware. It speaks
//! newline-delimited JSON-RPC on stdio and talks to the image over the LAN.
//!
//! `METAL_HOST` overrides the target, `METAL_CACHE` the build cache (which holds the
//! token, image, and TFTP root), and `METAL_NETCONSOLE` the UDP address for kernel logs.

use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    env, fs,
    io::{self, BufRead, Read, Write},
    net::{TcpStream, ToSocketAddrs, UdpSocket},
    path::PathBuf,
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

const MAX_LOG_LINES: usize = 20_000;
const MAX_REPLY_BYTES: u64 = 4 << 20;
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const DEFAULT_RUN_SECONDS: u64 = 60;
const MAX_RUN_SECONDS: u64 = 600;
const IMAGE: &str = "BOOTX64.EFI";

/// Read until the declared body has arrived, or the peer closes. A rebooting appliance
/// never closes, so waiting for the close alone would always time out.
fn read_reply(stream: &mut impl Read, reply: &mut Vec<u8>) -> io::Result<()> {
    let mut chunk = [0; 16 * 1024];
    loop {
        let len = stream.read(&mut chunk)?;
        if len == 0 {
            return Ok(());
        }
        reply.extend_from_slice(&chunk[..len]);
        let text = String::from_utf8_lossy(reply);
        if let Some((head, body)) = text.split_once("\r\n\r\n") {
            let declared = head.lines().find_map(|line| {
                line.to_ascii_lowercase()
                    .strip_prefix("content-length:")?
                    .trim()
                    .parse::<usize>()
                    .ok()
            });
            if declared.is_some_and(|declared| body.len() >= declared) {
                return Ok(());
            }
        }
    }
}

struct Server {
    host: String,
    cache: PathBuf,
    log: Arc<Mutex<VecDeque<String>>>,
}

fn main() -> io::Result<()> {
    let cache = env::var_os("METAL_CACHE")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".cache/chatty-metal")
        });
    let server = Server {
        host: env::var("METAL_HOST").unwrap_or_else(|_| "192.168.0.97:11434".to_owned()),
        cache,
        log: Arc::new(Mutex::new(VecDeque::new())),
    };
    let address = env::var("METAL_NETCONSOLE").unwrap_or_else(|_| "0.0.0.0:6666".to_owned());
    listen_netconsole(address, Arc::clone(&server.log));

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => server.dispatch(&request),
            Err(error) => Some(failure(Value::Null, -32700, &error.to_string())),
        };
        if let Some(response) = response {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

fn listen_netconsole(address: String, log: Arc<Mutex<VecDeque<String>>>) {
    thread::spawn(move || {
        let Ok(socket) = UdpSocket::bind(&address) else {
            push_log(
                &log,
                format!("[mcp] could not bind netconsole on {address}"),
            );
            return;
        };
        let mut datagram = [0; 2048];
        while let Ok((len, _)) = socket.recv_from(&mut datagram) {
            push_log(
                &log,
                String::from_utf8_lossy(&datagram[..len])
                    .trim_end()
                    .to_owned(),
            );
        }
    });
}

fn push_log(log: &Mutex<VecDeque<String>>, line: String) {
    let mut log = log.lock().unwrap_or_else(|poison| poison.into_inner());
    if log.len() == MAX_LOG_LINES {
        log.pop_front();
    }
    log.push_back(line);
}

fn success(id: Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn failure(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}

fn tools() -> Value {
    json!([
        tool(
            "metal_status",
            "Fetch the boot status text from the appliance. Needs no token.",
            json!({}),
            &[]
        ),
        tool(
            "metal_log",
            "Recent kernel log lines received over netconsole, including boots that never reached PID 1.",
            json!({"lines": {"type": "integer"}, "filter": {"type": "string"}}),
            &[]
        ),
        tool(
            "metal_dmesg",
            "The kernel log read from the running debug image.",
            json!({}),
            &[]
        ),
        tool(
            "metal_read",
            "Read a file or list a directory under /proc or /sys on the appliance.",
            json!({"path": {"type": "string"}}),
            &["path"]
        ),
        tool(
            "metal_run",
            "Upload a static x86-64 program from this computer, run it on the appliance without CAP_SYS_ADMIN, and return its output.",
            json!({"program": {"type": "string"}, "args": {"type": "string"}, "timeout_secs": {"type": "integer"}}),
            &["program"]
        ),
        tool(
            "metal_reboot",
            "Reboot the appliance. It boots the image again if armed, otherwise the next boot entry.",
            json!({}),
            &[]
        ),
        tool(
            "metal_boot",
            "Arm or disarm the network boot offer. Arming publishes the latest built image; disarming lets the host fall through to its normal OS.",
            json!({"armed": {"type": "boolean"}}),
            &["armed"]
        ),
    ])
}

impl Server {
    fn dispatch(&self, request: &Value) -> Option<Value> {
        let method = request["method"].as_str().unwrap_or("");
        // Requests without an id are notifications, which get no response.
        let id = request.get("id")?.clone();
        Some(match method {
            "initialize" => {
                let version = request["params"]["protocolVersion"]
                    .as_str()
                    .unwrap_or("2024-11-05");
                success(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "chatty-metal", "version": env!("CARGO_PKG_VERSION")},
                    }),
                )
            }
            "ping" => success(id, json!({})),
            "tools/list" => success(id, json!({"tools": tools()})),
            "tools/call" => {
                let name = request["params"]["name"].as_str().unwrap_or("");
                let arguments = &request["params"]["arguments"];
                let (text, is_error) = match self.call(name, arguments) {
                    Ok(text) => (text, false),
                    Err(error) => (error, true),
                };
                success(
                    id,
                    json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
                )
            }
            _ => failure(id, -32601, "unknown method"),
        })
    }

    fn call(&self, name: &str, arguments: &Value) -> Result<String, String> {
        match name {
            "metal_status" => self.http("GET", "/", None, "", &[], IO_TIMEOUT),
            "metal_log" => Ok(self.read_log(arguments)),
            "metal_dmesg" => self.http(
                "GET",
                "/debug/dmesg",
                Some(self.token()?),
                "",
                &[],
                IO_TIMEOUT,
            ),
            "metal_read" => {
                let path = arguments["path"].as_str().ok_or("path is required")?;
                if !path
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"/_.:-".contains(&byte))
                {
                    return Err("path has unsupported characters".to_owned());
                }
                self.http(
                    "GET",
                    &format!("/debug/read?path={path}"),
                    Some(self.token()?),
                    "",
                    &[],
                    IO_TIMEOUT,
                )
            }
            "metal_run" => {
                let program = arguments["program"].as_str().ok_or("program is required")?;
                let body = fs::read(program).map_err(|error| format!("{program}: {error}"))?;
                let args = arguments["args"].as_str().unwrap_or("");
                if args.contains(['\r', '\n']) {
                    return Err("args must be one line".to_owned());
                }
                let seconds = arguments["timeout_secs"]
                    .as_u64()
                    .unwrap_or(DEFAULT_RUN_SECONDS)
                    .clamp(1, MAX_RUN_SECONDS);
                let path = format!("/debug/run?timeout={seconds}");
                self.http(
                    "POST",
                    &path,
                    Some(self.token()?),
                    args,
                    &body,
                    Duration::from_secs(seconds + 30),
                )
            }
            "metal_reboot" => self.http(
                "POST",
                "/debug/reboot",
                Some(self.token()?),
                "",
                &[],
                IO_TIMEOUT,
            ),
            "metal_boot" => {
                self.set_armed(arguments["armed"].as_bool().ok_or("armed is required")?)
            }
            _ => Err(format!("unknown tool {name}")),
        }
    }

    fn read_log(&self, arguments: &Value) -> String {
        let limit = arguments["lines"]
            .as_u64()
            .unwrap_or(200)
            .min(MAX_LOG_LINES as u64) as usize;
        let filter = arguments["filter"].as_str().unwrap_or("");
        let log = self.log.lock().unwrap_or_else(|poison| poison.into_inner());
        let lines: Vec<&str> = log
            .iter()
            .rev()
            .filter(|line| line.contains(filter))
            .take(limit)
            .map(String::as_str)
            .collect();
        if lines.is_empty() {
            return "no matching log lines yet".to_owned();
        }
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }

    fn token(&self) -> Result<String, String> {
        let path = self.cache.join("debug-token");
        fs::read_to_string(&path)
            .map(|token| token.trim().to_owned())
            .map_err(|error| format!("{}: {error}; build with METAL_DEBUG=1", path.display()))
    }

    fn set_armed(&self, armed: bool) -> Result<String, String> {
        let root = self.cache.join("tftp");
        let target = root.join(IMAGE);
        if !armed {
            return match fs::remove_file(&target) {
                Ok(()) => Ok("disarmed".to_owned()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    Ok("already disarmed".to_owned())
                }
                Err(error) => Err(error.to_string()),
            };
        }
        let image = self.cache.join("out").join(IMAGE);
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        // Replace atomically, so a boot in progress never reads a half-copied image.
        let partial = root.join(".partial");
        let bytes =
            fs::copy(&image, &partial).map_err(|error| format!("{}: {error}", image.display()))?;
        fs::rename(&partial, &target).map_err(|error| error.to_string())?;
        Ok(format!(
            "armed with {bytes} bytes; a running netboot.sh will offer it"
        ))
    }

    fn http(
        &self,
        method: &str,
        path: &str,
        token: Option<String>,
        args: &str,
        body: &[u8],
        timeout: Duration,
    ) -> Result<String, String> {
        let describe = |error: io::Error| format!("{}: {error}", self.host);
        let address = self
            .host
            .to_socket_addrs()
            .map_err(describe)?
            .next()
            .ok_or("host did not resolve")?;
        let mut stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).map_err(describe)?;
        stream.set_read_timeout(Some(timeout)).map_err(describe)?;
        stream.set_write_timeout(Some(timeout)).map_err(describe)?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(token) = token {
            head.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        if !args.is_empty() {
            head.push_str(&format!("X-Args: {args}\r\n"));
        }
        head.push_str("\r\n");
        stream.write_all(head.as_bytes()).map_err(describe)?;
        stream.write_all(body).map_err(describe)?;

        let mut reply = Vec::new();
        read_reply(&mut stream.take(MAX_REPLY_BYTES), &mut reply).map_err(describe)?;
        let reply = String::from_utf8_lossy(&reply);
        let (head, text) = reply.split_once("\r\n\r\n").ok_or("malformed reply")?;
        if head.starts_with("HTTP/1.1 200") {
            Ok(text.to_owned())
        } else {
            Err(format!("{}: {text}", head.lines().next().unwrap_or("")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Yields its data, then fails like a socket whose peer vanished without closing.
    struct Vanishes(io::Cursor<Vec<u8>>);

    impl Read for Vanishes {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match self.0.read(buffer)? {
                0 => Err(io::ErrorKind::WouldBlock.into()),
                len => Ok(len),
            }
        }
    }

    #[test]
    fn reply_ends_at_declared_length_without_a_close() {
        let text = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec();
        let mut reply = Vec::new();
        read_reply(&mut Vanishes(io::Cursor::new(text.clone())), &mut reply).unwrap();
        assert_eq!(reply, text);
    }

    #[test]
    fn short_reply_still_waits_for_the_rest() {
        let text = b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nok".to_vec();
        let mut reply = Vec::new();
        assert!(read_reply(&mut Vanishes(io::Cursor::new(text)), &mut reply).is_err());
    }
}
