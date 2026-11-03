//! A connection kept open to home-link, which can then rest and wake this PC: its
//! `sleep` rests (every monitor but `keep` darkened, see `rest`) and its `wake` wakes.
//! The PC connects out, so nothing is opened for inbound connections. Messages follow
//! home-link's README ("PC").

use std::{sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::{net::TcpStream, time::Instant};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message};
use tracing::{info, warn};

use crate::{
    config::HomeLink,
    rest::{Rest, RestError},
};

/// A connection that lasted this long was up; the next failure starts over at 1 second.
const STEADY: Duration = Duration::from_mins(1);
/// home-link pings every 20 seconds; this long without a word means the line is gone
/// even if no close came (a dropped network).
const SILENCE: Duration = Duration::from_mins(1);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Keep connecting to home-link, waiting longer after each failure in a row.
pub async fn run(link: HomeLink, rest: Arc<Rest>) {
    let mut failures = 0;
    loop {
        let started = Instant::now();
        match tokio_tungstenite::connect_async(link.url.as_str()).await {
            Ok((socket, _)) => {
                info!(url = %link.url, name = %link.name, "connected to home-link");
                let reason = session(socket, &link, &rest).await;
                warn!(%reason, "disconnected from home-link");
            }
            Err(err) => warn!(url = %link.url, %err, "cannot connect to home-link"),
        }
        if started.elapsed() >= STEADY {
            failures = 0;
        }
        tokio::time::sleep(backoff(failures)).await;
        failures += 1;
    }
}

/// The wait before connecting again after `failures` failures in a row: 1 second,
/// doubling up to a minute.
fn backoff(failures: u32) -> Duration {
    Duration::from_secs(1u64 << failures.min(6)).min(Duration::from_mins(1))
}

/// A command's id and action, or `None` for any other message.
fn command(text: &str) -> Option<(Value, String)> {
    let message: Value = serde_json::from_str(text).ok()?;
    (message["type"] == "command").then(|| {
        let action = message["action"].as_str().unwrap_or_default().to_owned();
        (message["id"].clone(), action)
    })
}

/// Say hello, then answer commands and tell home-link when resting starts or ends
/// otherwise (the panel), until the connection ends; returns why it ended.
async fn session(mut socket: Socket, link: &HomeLink, rest: &Arc<Rest>) -> String {
    let mut changes = rest.subscribe();
    let mut told = *changes.borrow_and_update();
    let hello = json!({ "type": "hello", "name": link.name, "sleeping": told });
    if let Err(err) = socket.send(Message::text(hello.to_string())).await {
        return err.to_string();
    }
    let mut heard = Instant::now();
    loop {
        let reply = tokio::select! {
            message = socket.next() => {
                heard = Instant::now();
                let text = match message {
                    Some(Ok(Message::Text(text))) => text,
                    Some(Ok(Message::Close(_))) | None => return "closed".into(),
                    Some(Ok(_)) => continue,
                    Some(Err(err)) => return err.to_string(),
                };
                let Some((id, action)) = command(&text) else {
                    warn!(%text, "unexpected message from home-link");
                    continue;
                };
                let (rest, keep) = (rest.clone(), link.keep.clone());
                let done = tokio::task::spawn_blocking(move || obey(&rest, &keep, &action))
                    .await
                    .unwrap_or_else(|err| Err(RestError::Failed(err.to_string())));
                info!(%id, ?done, "home-link command");
                if let Ok(sleeping) = &done {
                    told = *sleeping;
                }
                result(&id, done, &link.keep)
            }
            // `rest` outlives the session, so this only ends with a change.
            _ = changes.changed() => {
                let now = *changes.borrow_and_update();
                if now == told {
                    continue;
                }
                told = now;
                json!({ "type": "state", "sleeping": now })
            }
            () = tokio::time::sleep_until(heard + SILENCE) => {
                return "home-link went silent".into();
            }
        };
        if let Err(err) = socket.send(Message::text(reply.to_string())).await {
            return err.to_string();
        }
    }
}

/// Rest on `sleep`, wake on `wake`; whether resting afterwards.
fn obey(rest: &Rest, keep: &str, action: &str) -> Result<bool, RestError> {
    match action {
        "sleep" => rest.rest(keep),
        "wake" => rest.wake(),
        other => Err(RestError::Failed(format!(
            "「{other}」には対応していません"
        ))),
    }
    .and_then(|()| rest.resting())
}

/// The answer to command `id`: whether resting afterwards, or why not for home-link's
/// screen.
fn result(id: &Value, done: Result<bool, RestError>, keep: &str) -> Value {
    let message = match done {
        Ok(sleeping) => {
            return json!({ "type": "result", "id": id, "ok": true, "sleeping": sleeping });
        }
        Err(RestError::UnknownMonitor) => format!("残すモニター {keep} がつながっていません"),
        Err(RestError::Unrestored(monitors)) => {
            format!("{} の明るさを戻せませんでした", monitors.join("、"))
        }
        Err(RestError::Failed(message)) => message,
    };
    json!({ "type": "result", "id": id, "ok": false, "message": message })
}

#[cfg(test)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use futures_util::{SinkExt, StreamExt};
    use serde_json::{Value, json};
    use tokio::net::{TcpListener, TcpStream};
    use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

    use super::{backoff, command, result, run};
    use crate::{
        config::HomeLink,
        rest::{Rest, RestError, fake},
    };

    #[test]
    fn waits_longer_after_each_failure_up_to_a_minute() {
        let waits: Vec<u64> = [0, 1, 2, 5, 6, 40]
            .map(|failures| backoff(failures).as_secs())
            .into();
        assert_eq!(waits, [1, 2, 4, 32, 60, 60]);
    }

    #[test]
    fn reads_commands_and_leaves_other_messages() {
        assert_eq!(
            command(r#"{"type":"command","id":7,"action":"sleep"}"#),
            Some((json!(7), "sleep".to_owned()))
        );
        assert_eq!(command(r#"{"type":"error","message":"bad hello"}"#), None);
        assert_eq!(command("not json"), None);
    }

    #[test]
    fn answers_whether_resting_or_why_not() {
        assert_eq!(
            result(&json!(1), Ok(true), "panel"),
            json!({ "type": "result", "id": 1, "ok": true, "sleeping": true })
        );
        let message = |err| result(&json!(2), Err(err), "mon-1")["message"].clone();
        assert_eq!(
            message(RestError::UnknownMonitor),
            "残すモニター mon-1 がつながっていません"
        );
        assert_eq!(
            message(RestError::Unrestored(vec!["MAIN".into(), "SIDE".into()])),
            "MAIN、SIDE の明るさを戻せませんでした"
        );
        assert_eq!(message(RestError::Failed("disk full".into())), "disk full");
        assert_eq!(
            result(&json!(2), Err(RestError::UnknownMonitor), "mon-1")["ok"],
            false
        );
    }

    /// A home-link on a free port, a PC connecting to it with `keep`, and its rest.
    async fn connected(
        keep: &str,
    ) -> (
        TcpListener,
        WebSocketStream<TcpStream>,
        Arc<Rest>,
        Arc<fake::FakeScreens>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let link = HomeLink {
            url: format!("ws://{}/api/pcs/connect", listener.local_addr().unwrap()),
            name: "ゲームPC".into(),
            keep: keep.into(),
        };
        let (rest, screens) = fake::rest();
        tokio::spawn(run(link, rest.clone()));
        let socket = accept(&listener).await;
        (listener, socket, rest, screens)
    }

    async fn accept(listener: &TcpListener) -> WebSocketStream<TcpStream> {
        let (stream, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
            .await
            .expect("the PC connects")
            .unwrap();
        tokio_tungstenite::accept_async(stream).await.unwrap()
    }

    async fn receive(socket: &mut WebSocketStream<TcpStream>) -> Value {
        loop {
            let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("the PC sends a message")
                .unwrap()
                .unwrap();
            if let Message::Text(text) = message {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }

    async fn order(socket: &mut WebSocketStream<TcpStream>, id: u64, action: &str) -> Value {
        let message = json!({ "type": "command", "id": id, "action": action });
        socket
            .send(Message::text(message.to_string()))
            .await
            .unwrap();
        receive(socket).await
    }

    #[tokio::test]
    async fn says_hello_then_rests_on_sleep_and_wakes_on_wake() {
        let (_listener, mut socket, rest, screens) = connected("panel").await;
        assert_eq!(
            receive(&mut socket).await,
            json!({ "type": "hello", "name": "ゲームPC", "sleeping": false })
        );
        assert_eq!(
            order(&mut socket, 1, "sleep").await,
            json!({ "type": "result", "id": 1, "ok": true, "sleeping": true })
        );
        assert!(rest.resting().unwrap());
        assert_eq!(screens.levels.lock().unwrap()["main"], 0);
        assert_eq!(
            order(&mut socket, 2, "wake").await,
            json!({ "type": "result", "id": 2, "ok": true, "sleeping": false })
        );
        assert_eq!(screens.levels.lock().unwrap()["main"], 40);
    }

    #[tokio::test]
    async fn a_sleep_that_cannot_rest_says_why() {
        let (_listener, mut socket, rest, _) = connected("gone").await;
        receive(&mut socket).await;
        let result = order(&mut socket, 3, "sleep").await;
        assert_eq!(
            (result["id"].clone(), result["ok"].clone()),
            (json!(3), json!(false))
        );
        assert!(
            result["message"].as_str().unwrap().contains("gone"),
            "names the monitor: {result}"
        );
        assert!(!rest.resting().unwrap());
        let result = order(&mut socket, 4, "reboot").await;
        assert_eq!(result["ok"], false);
    }

    #[tokio::test]
    async fn tells_resting_from_the_panel_and_connects_again_when_dropped() {
        let (listener, mut socket, rest, _) = connected("panel").await;
        receive(&mut socket).await;
        rest.rest("panel").unwrap();
        assert_eq!(
            receive(&mut socket).await,
            json!({ "type": "state", "sleeping": true })
        );
        drop(socket);
        let mut socket = accept(&listener).await;
        assert_eq!(
            receive(&mut socket).await,
            json!({ "type": "hello", "name": "ゲームPC", "sleeping": true })
        );
    }
}
