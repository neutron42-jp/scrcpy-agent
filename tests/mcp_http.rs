use std::io::{Read, Write};
use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{json, Value};

use scrcpy_agent::app::AppState;
use scrcpy_agent::config::Config;

fn post(addr: SocketAddr, payload: &Value) -> (u16, Option<Value>) {
    let mut stream = std::net::TcpStream::connect(addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let body = payload.to_string();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(request.as_bytes()).unwrap();
    stream.flush().unwrap();

    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let text = String::from_utf8_lossy(&response);
    let (headers, body) = text.split_once("\r\n\r\n").expect("http response");
    let status: u16 = headers
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let value = if body.trim().is_empty() {
        None
    } else {
        Some(serde_json::from_str(body).unwrap_or_else(|err| panic!("bad json ({err}): {body}")))
    };
    (status, value)
}

#[test]
fn mcp_lists_tools_and_reports_missing_session() {
    let state = AppState::new(Config::default());
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let addr = listener.local_addr().unwrap();
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let runtime = scrcpy_agent::mcp::runtime();
    let join = runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        scrcpy_agent::mcp::serve(listener, state, shutdown_rx)
            .await
            .unwrap();
    });

    let (status, initialize) = post(
        addr,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2026-07-28",
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"}
            }
        }),
    );
    assert_eq!(status, 200);
    let initialize = initialize.expect("initialize response");
    assert_eq!(initialize["result"]["protocolVersion"], "2026-07-28");
    assert_eq!(
        initialize["result"]["capabilities"]["tools"]["listChanged"],
        false
    );
    assert_eq!(initialize["result"]["serverInfo"]["name"], "android");

    let (status, list) = post(
        addr,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    assert_eq!(status, 200);
    let list = list.expect("tools/list response");
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert!(
        tools.len() >= 20,
        "expected the full tool set, got {}",
        tools.len()
    );
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    for expected in [
        "screenshot",
        "tap",
        "drag",
        "swipe",
        "type_text",
        "press_key",
        "scroll",
        "launch_app",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }

    let (status, call) = post(
        addr,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "screenshot", "arguments": {}}
        }),
    );
    assert_eq!(status, 200);
    let call = call.expect("tools/call response");
    assert_eq!(call["result"]["isError"], true);
    let text = call["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default();
    assert!(
        text.contains("no Android session"),
        "unexpected text: {text}"
    );

    let (status, notification) = post(
        addr,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!(status, 202);
    assert!(notification.is_none());

    let _ = shutdown_tx.send(());
    runtime.block_on(join).unwrap();
}
