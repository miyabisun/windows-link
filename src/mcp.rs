//! A Model Context Protocol server (Streamable HTTP, JSON replies only) for the mouse
//! board.
//! Each tool is a request to the HTTP API beside it, so it checks, behaves and fails
//! as the API does, without authentication like the API.

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use tower::ServiceExt;

/// The protocol versions spoken; the first is answered when the client asks for
/// another.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// Answers one JSON-RPC message posted to `/mcp`, calling `api` for the tools.
///
/// The board is a real mouse, so a web page must not reach the tools: a request that is
/// not sent as JSON is refused (a page can send other kinds to any site without asking
/// it first), and the server refuses one that says it comes from a page of another site
/// before it gets here.
pub async fn handle(api: Router, headers: HeaderMap, body: Bytes) -> Response {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|kind| kind.to_str().ok())
        .and_then(|kind| kind.split(';').next())
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"));
    if !json {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
        return reply(
            StatusCode::BAD_REQUEST,
            &Value::Null,
            Err((-32700, "parse error")),
        );
    };
    let Some(id) = message
        .get("id")
        .filter(|_| message.get("method").is_some())
    else {
        // A notification or a response: nothing to answer.
        return StatusCode::ACCEPTED.into_response();
    };
    let params = &message["params"];
    let result = match message["method"].as_str().unwrap_or_default() {
        "initialize" => {
            let asked = params["protocolVersion"].as_str().unwrap_or_default();
            Ok(json!({
                "protocolVersion": VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]),
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "windows-link", "version": env!("CARGO_PKG_VERSION") },
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call(api, params).await,
        _ => Err((-32601, "method not found")),
    };
    reply(StatusCode::OK, id, result)
}

fn reply(status: StatusCode, id: &Value, result: Result<Value, (i64, &str)>) -> Response {
    let body = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Calls the API for the tool in `params`; the API's answer is the tool's text, an
/// error status its error.
async fn call(api: Router, params: &Value) -> Result<Value, (i64, &'static str)> {
    // The path of the mouse's request; the tool's arguments are its body, the program
    // (`process`) included, so the caller says in which window the mouse works.
    let path = match params["name"].as_str().unwrap_or_default() {
        "list_buttons" => None,
        "mouse_click" => Some("/mouse/click"),
        "mouse_drag" => Some("/mouse/drag"),
        "mouse_wheel" => Some("/mouse/wheel"),
        _ => return Err((-32602, "unknown tool")),
    };
    let request = Request::builder().header(header::CONTENT_TYPE, "application/json");
    let request = match path {
        None => request.method("GET").uri("/buttons").body(Body::empty()),
        Some(path) => {
            let arguments = params["arguments"].as_object().cloned().unwrap_or_default();
            request
                .method("POST")
                .uri(path)
                .body(Body::from(Value::Object(arguments).to_string()))
        }
    }
    .map_err(|_| (-32603, "internal error"))?;
    let Ok(response) = api.oneshot(request).await;
    let failed = !response.status().is_success();
    let text = to_bytes(response.into_body(), usize::MAX)
        .await
        .map_or_else(
            |_| String::new(),
            |b| String::from_utf8_lossy(&b).into_owned(),
        );
    Ok(json!({
        "content": [{ "type": "text", "text": if text.is_empty() { "null".to_owned() } else { text } }],
        "isError": failed,
    }))
}

/// The tools with the schemas of their arguments.
fn tools() -> Value {
    let process = json!({ "type": "string",
        "description": "操作する窓を持つプログラムの実行ファイル名（例: BlueArchive.exe、notepad.exe）。動いているプログラムなら、設定に無いものも指定できる。パスは付けない" });
    let across = |what: &str| {
        json!({ "type": "number", "minimum": 0, "maximum": 1,
            "description": format!("{what}。窓の表示領域の左端を0、右端を1とした横の位置") })
    };
    let down = |what: &str| {
        json!({ "type": "number", "minimum": 0, "maximum": 1,
            "description": format!("{what}。窓の表示領域の上端を0、下端を1とした縦の位置") })
    };
    let button = json!({ "type": "string", "enum": ["left", "right", "middle"],
        "description": "押すボタン（省くと left）" });
    json!([
        { "name": "list_buttons",
          "description": "ボタンの一覧（id・type・label・state）。type が automation.hold のボタンの state.held が true の間（ユーザーが操作中）、マウスの道具は user_operating で失敗する",
          "inputSchema": { "type": "object", "properties": {} } },
        { "name": "mouse_click",
          "description": "process で指定したプログラムの窓の中の点を、本物のUSBマウス（マウスボード）でクリックする。その窓を前に出し、カーソルをその点へ移して押し、カーソルを元の位置へ戻す。管理者権限で動くプログラムにも届く。そのプログラムに窓が無いと no_window、ほかの窓がその点を覆っていると covered で失敗する",
          "inputSchema": { "type": "object", "required": ["process", "x", "y"], "properties": {
              "process": process, "x": across("クリックする点"), "y": down("クリックする点"), "button": button,
              "count": { "type": "integer", "minimum": 1, "maximum": 2,
                  "description": "押す回数（省くと1、2でダブルクリック）" } } } },
        { "name": "mouse_drag",
          "description": "process で指定したプログラムの窓の中で、点から点へボタンを押したままカーソルを動かして離す（一覧のスワイプ、つまみの移動など）",
          "inputSchema": { "type": "object", "required": ["process", "x", "y", "to_x", "to_y"], "properties": {
              "process": process, "x": across("押す点"), "y": down("押す点"),
              "to_x": across("離す点"), "to_y": down("離す点"), "button": button } } },
        { "name": "mouse_wheel",
          "description": "process で指定したプログラムの窓の中の点で、マウスのホイールを回す",
          "inputSchema": { "type": "object", "required": ["process", "x", "y", "notches"], "properties": {
              "process": process, "x": across("ホイールを回す点"), "y": down("ホイールを回す点"),
              "notches": { "type": "integer", "minimum": -20, "maximum": 20,
                  "description": "回す段数（1〜20。負の数で下へスクロール）" } } } },
    ])
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::{
        audio::fake::FakeAudio,
        config,
        launch::fake::FakeLauncher,
        mouse::{Motion, MouseButton, fake::FakePointer},
        server::{AppState, app},
    };

    const CONFIG: &str = "buttons:
  - id: ba
    label: BA
    type: app.launch
    target: launcher.exe
    process: BlueArchive.exe
  - id: hands
    label: Hands
    type: automation.hold
";

    fn state() -> (AppState, Arc<FakePointer>) {
        let pointer = Arc::new(FakePointer::default());
        let state = AppState::new(
            config::parse(CONFIG).unwrap(),
            Arc::new(FakeAudio::new(Vec::new(), None)),
        )
        .with_launcher(Arc::new(FakeLauncher::default()))
        .with_pointer(pointer.clone());
        (state, pointer)
    }

    /// Post `body` to `/mcp` as JSON, with an `Origin` when given.
    async fn post(state: AppState, origin: Option<&str>, body: String) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "application/json");
        if let Some(origin) = origin {
            request = request.header("origin", origin);
        }
        let response = app(state)
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
    }

    async fn rpc(state: AppState, method: &str, params: Value) -> Value {
        let message = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        let (status, body) = post(state, None, message.to_string()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["id"], 7);
        body
    }

    /// A tool's answer: whether it failed, and its text read as JSON.
    async fn tool(state: AppState, name: &str, arguments: Value) -> (bool, Value) {
        let body = rpc(
            state,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await;
        let result = &body["result"];
        let text = result["content"][0]["text"].as_str().unwrap();
        (
            result["isError"].as_bool().unwrap(),
            serde_json::from_str(text).unwrap(),
        )
    }

    #[tokio::test]
    async fn a_client_is_greeted_with_a_version_it_asked_for_or_the_newest() {
        let (state, _) = state();
        let body = rpc(
            state.clone(),
            "initialize",
            json!({ "protocolVersion": "2025-03-26" }),
        )
        .await;
        assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(body["result"]["serverInfo"]["name"], "windows-link");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        let body = rpc(
            state,
            "initialize",
            json!({ "protocolVersion": "1999-01-01" }),
        )
        .await;
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
    }

    #[tokio::test]
    async fn the_tools_are_listed_with_what_each_one_needs() {
        let (state, _) = state();
        let body = rpc(state, "tools/list", json!({})).await;
        let tools = body["result"]["tools"].as_array().unwrap();
        let needs = |name: &str| {
            let tool = tools.iter().find(|tool| tool["name"] == name).unwrap();
            assert!(!tool["description"].as_str().unwrap().is_empty(), "{name}");
            tool["inputSchema"]["required"].clone()
        };
        assert_eq!(needs("list_buttons"), Value::Null);
        assert_eq!(needs("mouse_click"), json!(["process", "x", "y"]));
        assert_eq!(
            needs("mouse_drag"),
            json!(["process", "x", "y", "to_x", "to_y"])
        );
        assert_eq!(
            needs("mouse_wheel"),
            json!(["process", "x", "y", "notches"])
        );
        assert_eq!(tools.len(), 4);
    }

    #[tokio::test]
    async fn each_mouse_tool_does_what_its_api_request_does() {
        let (state, pointer) = state();
        let (failed, buttons) = tool(state.clone(), "list_buttons", json!({})).await;
        assert!(!failed);
        assert_eq!(buttons[0]["id"], "ba");

        let click = json!({ "process": "BlueArchive.exe", "x": 0.25, "y": 0.5, "button": "right" });
        let (failed, answer) = tool(state.clone(), "mouse_click", click).await;
        assert!(!failed);
        assert_eq!(answer, Value::Null, "the API answers 204 with nothing");
        let drag =
            json!({ "process": "BlueArchive.exe", "x": 0.5, "y": 0.8, "to_x": 0.5, "to_y": 0.2 });
        assert!(!tool(state.clone(), "mouse_drag", drag).await.0);
        // The caller names the program: one that no button of the configuration starts
        // is reached as well.
        let wheel = json!({ "process": "notepad.exe", "x": 0.5, "y": 0.5, "notches": -3 });
        assert!(!tool(state, "mouse_wheel", wheel).await.0);

        let done = pointer.done.lock().unwrap();
        assert_eq!(done.len(), 3);
        assert_eq!(done[0].process, "BlueArchive.exe");
        assert_eq!(
            done[0].motion,
            Motion::Click {
                button: MouseButton::Right,
                count: 1
            }
        );
        assert_eq!((done[0].x, done[0].y), (0.25, 0.5));
        assert_eq!(
            done[1].motion,
            Motion::Drag {
                to_x: 0.5,
                to_y: 0.2,
                button: MouseButton::Left
            }
        );
        assert_eq!(done[2].process, "notepad.exe");
        assert_eq!(done[2].motion, Motion::Wheel { notches: -3 });
    }

    #[tokio::test]
    async fn a_tool_fails_as_its_api_request_does() {
        let (state, pointer) = state();
        let click = json!({ "process": "BlueArchive.exe", "x": 0.5, "y": 0.5 });
        // While the user holds the PC, the mouse stays still.
        let pressed = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/buttons/hands/press")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pressed.status(), StatusCode::OK);
        let (failed, answer) = tool(state.clone(), "mouse_click", click).await;
        assert!(failed);
        assert_eq!(answer["error"], "user_operating");
        assert!(pointer.done.lock().unwrap().is_empty());

        // Let go again: what follows fails for its own reason.
        let pressed = app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/buttons/hands/press")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(pressed.status(), StatusCode::OK);

        let (failed, answer) = tool(
            state.clone(),
            "mouse_click",
            json!({ "process": "C:\\Games\\BlueArchive.exe", "x": 0.5, "y": 0.5 }),
        )
        .await;
        assert!(failed, "a program is named by its file name");
        assert_eq!(answer["error"], "invalid_click");

        // Without a program the API refuses the request, and says what is missing.
        let body = rpc(
            state.clone(),
            "tools/call",
            json!({ "name": "mouse_click", "arguments": { "x": 0.5, "y": 0.5 } }),
        )
        .await;
        assert_eq!(body["result"]["isError"], true);
        let text = body["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("process"), "{text}");
        assert!(pointer.done.lock().unwrap().is_empty());

        let body = rpc(state.clone(), "tools/call", json!({ "name": "sleep" })).await;
        assert_eq!(body["error"]["code"], -32602, "no such tool");
        let body = rpc(state, "resources/list", json!({})).await;
        assert_eq!(body["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn a_notification_is_taken_without_an_answer() {
        let (state, _) = state();
        let message = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        let (status, body) = post(state.clone(), None, message.to_string()).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body, Value::Null);
        let (status, body) = post(state, None, "not json".into()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], -32700);
    }

    /// A page of another web site cannot have the mouse used: a browser sends where the
    /// page is from, and sends anything but JSON without asking first.
    #[tokio::test]
    async fn a_web_page_of_another_site_cannot_use_the_tools() {
        let (state, pointer) = state();
        let click = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {
            "name": "mouse_click",
            "arguments": { "process": "BlueArchive.exe", "x": 0.5, "y": 0.5 } } })
        .to_string();
        let (status, _) = post(state.clone(), Some("https://example.com"), click.clone()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);

        let form = Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("content-type", "text/plain")
            .body(Body::from(click.clone()))
            .unwrap();
        let response = app(state.clone()).oneshot(form).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert!(pointer.done.lock().unwrap().is_empty());

        // The panel's own page, and a client that is no browser, are let through.
        let (status, _) = post(state.clone(), Some("http://tauri.localhost"), click.clone()).await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = post(state, None, click).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(pointer.done.lock().unwrap().len(), 2);
    }
}
