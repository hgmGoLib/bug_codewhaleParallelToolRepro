//! Deterministic repro: Codewhale (anthropic provider) sends a fake
//! "tool call was not executed" result for the 2nd of two parallel tool calls.
//!
//! One command, no API key, no real model:
//!
//!     cargo run -- [path/to/codewhale]
//!
//! What it does:
//!   1. starts a scripted fake Anthropic Messages API on 127.0.0.1:<random port>
//!      - request without tool results  -> one assistant message with TWO parallel
//!        `tool_use` blocks: read{path:"a.txt"} and read{path:"b.txt"}
//!      - request whose last message has tool results -> final text, end_turn
//!   2. creates a temp dir with an isolated CODEWHALE_HOME (config.toml pointing the
//!      `anthropic` provider at the fake server) and a work dir with a.txt / b.txt
//!   3. runs `codewhale --config <cfg> exec --auto --output-format stream-json <prompt>` once
//!   4. saves every request body as req_<n>.json and checks that every `tool_use`
//!      is answered by exactly one `tool_result`
//!
//! Exit code: 1 = bug reproduced, 0 = history is correct, 2 = setup problem.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PLACEHOLDER: &str = "tool call was not executed";
const PROMPT: &str = "Read a.txt and b.txt and tell me what they say.";

fn main() {
    let bin = find_codewhale();
    println!("codewhale binary: {}", bin.display());
    match Command::new(&bin).arg("--version").output() {
        Ok(o) => println!("{}", String::from_utf8_lossy(&o.stdout).trim()),
        Err(e) => fail_setup(&format!("cannot run {}: {e}", bin.display())),
    }

    // temp dir layout: <tmp>/home/config.toml, <tmp>/work/{a,b}.txt, <tmp>/req_<n>.json
    let root = std::env::temp_dir().join(format!("codewhale-parallel-tool-repro-{}", std::process::id()));
    let home = root.join("home");
    let work = root.join("work");
    for d in [&home, &work] {
        std::fs::create_dir_all(d).unwrap_or_else(|e| fail_setup(&format!("mkdir {}: {e}", d.display())));
    }
    std::fs::write(work.join("a.txt"), "content of file A\n").unwrap();
    std::fs::write(work.join("b.txt"), "content of file B\n").unwrap();

    // fake server
    let listener = TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| fail_setup(&format!("bind: {e}")));
    let port = listener.local_addr().unwrap().port();
    let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let requests = requests.clone();
        let root = root.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let requests = requests.clone();
                let root = root.clone();
                std::thread::spawn(move || handle_conn(stream, &requests, &root));
            }
        });
    }
    println!("fake Anthropic Messages API: http://127.0.0.1:{port}");

    let cfg = home.join("config.toml");
    std::fs::write(
        &cfg,
        format!(
            r#"provider = "anthropic"
approval_policy = "never"
sandbox_mode = "danger-full-access"
telemetry = false

[update]
check_for_updates = false

[providers.anthropic]
api_key = "sk-ant-fake-key-not-checked"
base_url = "http://127.0.0.1:{port}"
model = "claude-sonnet-4-6"
"#
        ),
    )
    .unwrap();
    println!("temp dir: {}", root.display());

    // run codewhale once
    let log = std::fs::File::create(root.join("codewhale.log")).unwrap();
    let mut child = Command::new(&bin)
        .arg("--config")
        .arg(&cfg)
        .args(["exec", "--auto", "--output-format", "stream-json", PROMPT])
        .current_dir(&work)
        .env("CODEWHALE_HOME", &home)
        .env("CODEWHALE_TELEMETRY", "0")
        .env("CODEWHALE_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone().unwrap())
        .stderr(log)
        .spawn()
        .unwrap_or_else(|e| fail_setup(&format!("spawn codewhale: {e}")));
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        if let Some(st) = child.try_wait().unwrap() {
            println!("codewhale exited: {st}");
            break;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            println!("codewhale timed out after 180s (killed)");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    // verdict
    let reqs = requests.lock().unwrap().clone();
    println!("\nrequests received: {}", reqs.len());
    let mut reproduced = false;
    for (i, body) in reqs.iter().enumerate() {
        reproduced |= check_history(i + 1, body);
    }
    println!();
    if reproduced {
        println!(
            ">>> BUG REPRODUCED: a tool call that was executed also got a '{PLACEHOLDER}' \
             placeholder result (contradictory history sent to the model)"
        );
        println!("request bodies + codewhale.log: {}", root.display());
        std::process::exit(1);
    }
    if !reqs.iter().any(|b| last_message_has_tool_result(b)) {
        fail_setup(&format!(
            "codewhale never sent the tool results back; see {}",
            root.join("codewhale.log").display()
        ));
    }
    println!("OK: every tool_use is answered by exactly one tool_result");
}

fn fail_setup(msg: &str) -> ! {
    eprintln!("setup problem: {msg}");
    std::process::exit(2);
}

/// codewhale binary: argv[1] > $CODEWHALE_BIN > npm global install (Windows) > PATH.
fn find_codewhale() -> PathBuf {
    if let Some(p) = std::env::args().nth(1) {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("CODEWHALE_BIN") {
        return PathBuf::from(p);
    }
    // `npm install -g codewhale` on Windows: PATH only has a .cmd shim, the real binary is here.
    if let Ok(appdata) = std::env::var("APPDATA") {
        let p = Path::new(&appdata).join(r"npm\node_modules\codewhale\bin\downloads\codewhale.exe");
        if p.exists() {
            return p;
        }
    }
    PathBuf::from("codewhale")
}

// ---------------------------- fake Anthropic Messages API ----------------------------

fn handle_conn(stream: TcpStream, requests: &Mutex<Vec<Value>>, root: &Path) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    // one request per connection (we answer with Connection: close)
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    let mut chunked = false;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h).unwrap_or(0) == 0 || h == "\r\n" || h == "\n" {
            break;
        }
        let lower = h.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            content_length = v.trim().parse().unwrap_or(0);
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    let raw = if chunked { read_chunked(&mut reader) } else {
        let mut b = vec![0u8; content_length];
        let _ = reader.read_exact(&mut b);
        b
    };

    if method != "POST" || !path.trim_end_matches('/').ends_with("/messages") {
        println!("{method} {path} -> 404");
        let body = r#"{"type":"error","error":{"type":"not_found_error","message":"not found"}}"#;
        respond(&mut out, "404 Not Found", "application/json", body.as_bytes());
        return;
    }
    let body: Value = match serde_json::from_slice(&raw) {
        Ok(v) => v,
        Err(e) => {
            println!("POST {path}: bad json: {e}");
            respond(&mut out, "400 Bad Request", "application/json", b"{}");
            return;
        }
    };
    let (n, tool_turn) = {
        let mut reqs = requests.lock().unwrap();
        reqs.push(body.clone());
        let tool_turn = reqs.iter().filter(|b| wants_tool_calls(b)).count();
        (reqs.len(), tool_turn)
    };
    let _ = std::fs::write(root.join(format!("req_{n}.json")), &raw);
    let messages = body["messages"].as_array().map_or(0, |m| m.len());
    let tools = body["tools"].as_array().map_or(0, |t| t.len());
    println!("POST {path} req_{n}: stream={} messages={messages} tools={tools}", body["stream"]);

    let (blocks, stop) = build_reply(&body, tool_turn);
    let model = body["model"].as_str().unwrap_or("claude-sonnet-4-6");
    let id = format!("msg_repro_{n}");
    if body["stream"].as_bool() != Some(true) {
        let msg = json!({"id": id, "type": "message", "role": "assistant", "model": model,
            "content": blocks, "stop_reason": stop, "stop_sequence": null,
            "usage": {"input_tokens": 10, "output_tokens": 10}});
        respond(&mut out, "200 OK", "application/json", msg.to_string().as_bytes());
        return;
    }
    let mut sse = String::new();
    let mut ev = |name: &str, data: Value| sse.push_str(&format!("event: {name}\ndata: {data}\n\n"));
    ev("message_start", json!({"type": "message_start", "message": {"id": id, "type": "message",
        "role": "assistant", "model": model, "content": [], "stop_reason": null, "stop_sequence": null,
        "usage": {"input_tokens": 10, "output_tokens": 1}}}));
    for (i, b) in blocks.iter().enumerate() {
        if b["type"] == "text" {
            ev("content_block_start", json!({"type": "content_block_start", "index": i,
                "content_block": {"type": "text", "text": ""}}));
            ev("content_block_delta", json!({"type": "content_block_delta", "index": i,
                "delta": {"type": "text_delta", "text": b["text"]}}));
        } else {
            ev("content_block_start", json!({"type": "content_block_start", "index": i,
                "content_block": {"type": "tool_use", "id": b["id"], "name": b["name"], "input": {}}}));
            ev("content_block_delta", json!({"type": "content_block_delta", "index": i,
                "delta": {"type": "input_json_delta", "partial_json": b["input"].to_string()}}));
        }
        ev("content_block_stop", json!({"type": "content_block_stop", "index": i}));
    }
    ev("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null},
        "usage": {"output_tokens": 10}}));
    ev("message_stop", json!({"type": "message_stop"}));
    respond(&mut out, "200 OK", "text/event-stream", sse.as_bytes());
}

fn read_chunked(r: &mut impl BufRead) -> Vec<u8> {
    let mut body = Vec::new();
    loop {
        let mut size = String::new();
        if r.read_line(&mut size).unwrap_or(0) == 0 {
            break;
        }
        let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16).unwrap_or(0);
        if n == 0 {
            let mut crlf = String::new();
            let _ = r.read_line(&mut crlf);
            break;
        }
        let mut chunk = vec![0u8; n + 2];
        if r.read_exact(&mut chunk).is_err() {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body
}

fn respond(out: &mut TcpStream, status: &str, ctype: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(body);
    let _ = out.flush();
}

/// A request the fake model answers with the two parallel tool calls.
fn wants_tool_calls(body: &Value) -> bool {
    read_tool_name(body).is_some() && !last_message_has_tool_result(body)
}

fn read_tool_name(body: &Value) -> Option<String> {
    let names: Vec<&str> = body["tools"].as_array()?.iter().filter_map(|t| t["name"].as_str()).collect();
    ["read", "read_file", "Read"].iter().find(|w| names.contains(w)).map(|s| s.to_string())
}

fn last_message_has_tool_result(body: &Value) -> bool {
    body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .and_then(|m| m["content"].as_array())
        .is_some_and(|c| c.iter().any(|b| b["type"] == "tool_result"))
}

fn build_reply(body: &Value, tool_turn: usize) -> (Vec<Value>, &'static str) {
    if last_message_has_tool_result(body) {
        return (vec![json!({"type": "text", "text": "I read both files. Done."})], "end_turn");
    }
    let Some(read) = read_tool_name(body) else {
        return (vec![json!({"type": "text", "text": "ok"})], "end_turn");
    };
    (
        vec![
            json!({"type": "text", "text": "I will read both files in parallel."}),
            json!({"type": "tool_use", "id": format!("toolu_repro_{tool_turn}_a"), "name": read, "input": {"path": "a.txt"}}),
            json!({"type": "tool_use", "id": format!("toolu_repro_{tool_turn}_b"), "name": read, "input": {"path": "b.txt"}}),
        ],
        "tool_use",
    )
}

// ---------------------------- checker ----------------------------

/// Prints every tool_use in the request history with all tool_results answering it.
/// Returns true if some tool_use has more than one result or a placeholder result.
fn check_history(n: usize, body: &Value) -> bool {
    let Some(messages) = body["messages"].as_array() else { return false };
    let mut uses = Vec::new(); // (message index, id, name)
    for (i, m) in messages.iter().enumerate() {
        if m["role"] != "assistant" {
            continue;
        }
        for b in m["content"].as_array().into_iter().flatten() {
            if b["type"] == "tool_use" {
                uses.push((i, b["id"].as_str().unwrap_or("").to_string(), b["name"].as_str().unwrap_or("").to_string()));
            }
        }
    }
    if uses.is_empty() {
        return false;
    }
    let roles: Vec<&str> = messages.iter().map(|m| m["role"].as_str().unwrap_or("?")).collect();
    println!("[req_{n}] message roles: {roles:?}");
    let mut bug = false;
    for (i, id, name) in &uses {
        let mut results = Vec::new(); // (message index, is_error, text)
        for (j, m) in messages.iter().enumerate() {
            if m["role"] != "user" {
                continue;
            }
            for b in m["content"].as_array().into_iter().flatten() {
                if b["type"] == "tool_result" && b["tool_use_id"] == id.as_str() {
                    let text = match &b["content"] {
                        Value::String(s) => s.clone(),
                        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join(" "),
                        other => other.to_string(),
                    };
                    results.push((j, b["is_error"].as_bool().unwrap_or(false), text));
                }
            }
        }
        println!("[req_{n}]   tool_use {id} ({name}) in messages[{i}] -> {} tool_result(s)", results.len());
        for (j, is_err, text) in &results {
            println!("[req_{n}]     messages[{j}] is_error={is_err} content={:?}", text.chars().take(80).collect::<String>());
        }
        if results.len() > 1 || results.iter().any(|(_, _, t)| t == PLACEHOLDER) {
            bug = true;
        }
    }
    bug
}
