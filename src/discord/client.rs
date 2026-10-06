//! A connection to the Discord desktop app over its named pipe: commands wait for their
//! reply, events arrive on a queue, and the connection ends when Discord closes it.

use std::{
    collections::HashMap,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient},
    sync::{Mutex, mpsc, oneshot},
    time::timeout,
};
use tracing::{debug, info};

use super::{
    proto::{self, CODE_INVALID_TOKEN, CODE_REJECTED, Incoming, RpcError},
    token::{self, GrantError, Token},
};
use crate::secrets::DiscordApp;

const READY_TIMEOUT: Duration = Duration::from_secs(10);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(15);
/// How long the user has to answer Discord's authorization dialog.
const AUTHORIZE_TIMEOUT: Duration = Duration::from_mins(3);
/// The only scope needed: reading the servers and their icons.
pub const SCOPES: [&str; 1] = ["rpc"];

type Pending = Arc<Mutex<HashMap<String, oneshot::Sender<Result<Value, RpcError>>>>>;

#[derive(Debug)]
pub enum ConnectError {
    NotRunning,
    Failed(String),
}

impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotRunning => f.write_str("Discord is not running"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

#[derive(Debug)]
pub enum CommandError {
    Rpc(RpcError),
    Disconnected,
    Timeout,
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(error) => error.fmt(f),
            Self::Disconnected => f.write_str("Discord closed the connection"),
            Self::Timeout => f.write_str("Discord did not answer in time"),
        }
    }
}

pub struct Client {
    writer: Arc<Mutex<WriteHalf<NamedPipeClient>>>,
    pending: Pending,
    events: mpsc::UnboundedReceiver<(String, Value)>,
    nonce: AtomicU64,
}

async fn write_frame(
    writer: &Mutex<WriteHalf<NamedPipeClient>>,
    op: u32,
    payload: &Value,
) -> std::io::Result<()> {
    let mut writer = writer.lock().await;
    writer.write_all(&proto::encode(op, payload)).await?;
    writer.flush().await
}

async fn read_frame(reader: &mut ReadHalf<NamedPipeClient>) -> Result<(u32, Value), String> {
    let mut head = [0u8; 8];
    reader
        .read_exact(&mut head)
        .await
        .map_err(|err| err.to_string())?;
    let (op, len) = proto::header(head)?;
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        .await
        .map_err(|err| err.to_string())?;
    let value = serde_json::from_slice(&body).map_err(|err| err.to_string())?;
    Ok((op, value))
}

fn open_pipe() -> Result<NamedPipeClient, ConnectError> {
    let mut last = None;
    for n in 0..10 {
        match ClientOptions::new().open(format!(r"\\?\pipe\discord-ipc-{n}")) {
            Ok(pipe) => return Ok(pipe),
            Err(err) => last = Some(err),
        }
    }
    match last {
        Some(err) if err.kind() == std::io::ErrorKind::NotFound => Err(ConnectError::NotRunning),
        Some(err) => Err(ConnectError::Failed(format!(
            "cannot open Discord's pipe: {err}"
        ))),
        None => Err(ConnectError::NotRunning),
    }
}

impl Client {
    /// Connect and complete the handshake as `client_id`.
    pub async fn connect(client_id: &str) -> Result<Self, ConnectError> {
        let pipe = open_pipe()?;
        let (mut reader, writer) = tokio::io::split(pipe);
        let writer = Arc::new(Mutex::new(writer));
        let pending: Pending = Arc::default();
        let (events_tx, events) = mpsc::unbounded_channel();

        let reader_writer = writer.clone();
        let reader_pending = pending.clone();
        tokio::spawn(async move {
            loop {
                let (op, message) = match read_frame(&mut reader).await {
                    Ok(frame) => frame,
                    Err(err) => {
                        debug!(%err, "Discord pipe closed");
                        break;
                    }
                };
                match op {
                    proto::OP_PING => {
                        let _ = write_frame(&reader_writer, proto::OP_PONG, &message).await;
                    }
                    proto::OP_CLOSE => {
                        info!(reason = %message, "Discord closed the RPC connection");
                        break;
                    }
                    _ => match proto::classify(&message) {
                        Some(Incoming::Reply { nonce, result }) => {
                            if let Some(reply) = reader_pending.lock().await.remove(&nonce) {
                                let _ = reply.send(result);
                            }
                        }
                        Some(Incoming::Event { evt, data }) => {
                            let _ = events_tx.send((evt, data));
                        }
                        None => {}
                    },
                }
            }
            // Dropping the senders wakes every waiting command with `Disconnected`.
            reader_pending.lock().await.clear();
        });

        let mut client = Self {
            writer,
            pending,
            events,
            nonce: AtomicU64::new(0),
        };
        write_frame(
            &client.writer,
            proto::OP_HANDSHAKE,
            &proto::handshake(client_id),
        )
        .await
        .map_err(|err| ConnectError::Failed(format!("handshake failed: {err}")))?;
        match timeout(READY_TIMEOUT, client.events.recv()).await {
            Ok(Some((evt, _))) if evt == "READY" => Ok(client),
            Ok(Some((evt, data))) => Err(ConnectError::Failed(format!(
                "Discord answered the handshake with {evt}: {data}"
            ))),
            Ok(None) => Err(ConnectError::Failed(
                "Discord closed the connection during the handshake (is the client ID right?)"
                    .into(),
            )),
            Err(_) => Err(ConnectError::Failed(
                "Discord did not finish the handshake".into(),
            )),
        }
    }

    pub async fn command_with(
        &self,
        cmd: &str,
        args: Value,
        evt: Option<&str>,
        wait: Duration,
    ) -> Result<Value, CommandError> {
        let nonce = format!("wl-{}", self.nonce.fetch_add(1, Ordering::Relaxed));
        let (reply_tx, reply) = oneshot::channel();
        self.pending.lock().await.insert(nonce.clone(), reply_tx);
        if write_frame(
            &self.writer,
            proto::OP_FRAME,
            &proto::command(cmd, &args, evt, &nonce),
        )
        .await
        .is_err()
        {
            self.pending.lock().await.remove(&nonce);
            return Err(CommandError::Disconnected);
        }
        match timeout(wait, reply).await {
            Ok(Ok(result)) => result.map_err(CommandError::Rpc),
            Ok(Err(_)) => Err(CommandError::Disconnected),
            Err(_) => {
                self.pending.lock().await.remove(&nonce);
                Err(CommandError::Timeout)
            }
        }
    }

    pub async fn command(&self, cmd: &str, args: Value) -> Result<Value, CommandError> {
        self.command_with(cmd, args, None, COMMAND_TIMEOUT).await
    }

    pub async fn next_event(&mut self) -> Option<(String, Value)> {
        self.events.recv().await
    }
}

#[derive(Debug)]
pub enum SignInError {
    /// No usable token, and asking the user was not allowed this time.
    NeedsApproval,
    /// The user turned down Discord's authorization dialog.
    Rejected,
    Failed(String),
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeedsApproval => f.write_str(
                "windows-link needs approval in Discord: run `windows-link discord-servers`",
            ),
            Self::Rejected => f.write_str("the authorization was turned down in Discord"),
            Self::Failed(message) => f.write_str(message),
        }
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SignInError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|err| SignInError::Failed(err.to_string()))
}

/// A token that is valid for a while yet: the saved one, refreshed if it is close to
/// expiring, or a new one from Discord's authorization dialog when `may_ask`.
async fn usable_token(
    client: &Client,
    app: &DiscordApp,
    path: &Path,
    may_ask: bool,
) -> Result<Token, SignInError> {
    if let Some(saved) = token::load(path) {
        if !saved.needs_refresh(token::now()) {
            return Ok(saved);
        }
        let (app2, saved2) = (app.clone(), saved.clone());
        match blocking(move || token::refresh(&app2, &saved2)).await? {
            Ok(fresh) => {
                token::save(path, &fresh).map_err(SignInError::Failed)?;
                info!("refreshed the Discord token");
                return Ok(fresh);
            }
            Err(GrantError::Rejected(message)) => {
                info!(%message, "the saved Discord token cannot be refreshed");
                token::forget(path);
            }
            // Still valid for up to a day; try again next time.
            Err(GrantError::Failed(message)) if saved.expires_at > token::now() => {
                info!(%message, "cannot refresh the Discord token yet");
                return Ok(saved);
            }
            Err(GrantError::Failed(message)) => return Err(SignInError::Failed(message)),
        }
    }
    if !may_ask {
        return Err(SignInError::NeedsApproval);
    }
    info!("asking for approval in Discord");
    let code = match client
        .command_with(
            "AUTHORIZE",
            json!({ "client_id": app.client_id, "scopes": SCOPES }),
            None,
            AUTHORIZE_TIMEOUT,
        )
        .await
    {
        Ok(data) => data["code"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| SignInError::Failed("Discord returned no authorization code".into()))?,
        Err(CommandError::Rpc(error)) if error.code == CODE_REJECTED => {
            return Err(SignInError::Rejected);
        }
        Err(err) => return Err(SignInError::Failed(err.to_string())),
    };
    let app2 = app.clone();
    let fresh = blocking(move || token::exchange(&app2, &code))
        .await?
        .map_err(|err| SignInError::Failed(err.to_string()))?;
    token::save(path, &fresh).map_err(SignInError::Failed)?;
    Ok(fresh)
}

/// Authenticate the connection, asking the user in Discord only when `may_ask`.
pub async fn sign_in(
    client: &Client,
    app: &DiscordApp,
    path: &Path,
    may_ask: bool,
) -> Result<(), SignInError> {
    for _ in 0..2 {
        let token = usable_token(client, app, path, may_ask).await?;
        match client
            .command(
                "AUTHENTICATE",
                json!({ "access_token": token.access_token }),
            )
            .await
        {
            Ok(_) => return Ok(()),
            // Revoked or replaced elsewhere: drop it and get a new one once.
            Err(CommandError::Rpc(error)) if CODE_INVALID_TOKEN.contains(&error.code) => {
                info!(%error, "Discord no longer accepts the saved token");
                token::forget(path);
            }
            Err(err) => return Err(SignInError::Failed(err.to_string())),
        }
    }
    Err(SignInError::NeedsApproval)
}
