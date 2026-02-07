use std::path::PathBuf;

use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::{OwnedRoomId, UserId};
use matrix_sdk::{Client, Room, RoomState};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use crate::config::MatrixConfig;
use crate::error::{KleviathanError, KleviathanResult};

#[derive(Debug, Clone)]
pub struct IncomingMessage {
    pub sender: String,
    pub text: String,
}

#[derive(Serialize, Deserialize)]
struct PersistedSession {
    session: matrix_sdk::authentication::matrix::MatrixSession,
    sync_token: Option<String>,
}

pub struct MatrixConnector {
    client: Client,
    room_id: OwnedRoomId,
    incoming_rx: mpsc::Receiver<IncomingMessage>,
    enable_matrix_logs: bool,
}

impl MatrixConnector {
    pub async fn new(config: &MatrixConfig) -> KleviathanResult<Self> {
        let client = build_client(config).await?;
        login_or_restore(&client, config).await?;

        client
            .sync_once(SyncSettings::default())
            .await
            .map_err(|e| KleviathanError::Matrix(format!("Initial sync failed: {e}")))?;

        let room_id = resolve_dm_room(&client, &config.allowed_sender).await?;
        ensure_room_encrypted(&client, &room_id).await?;

        let (incoming_tx, incoming_rx) = mpsc::channel(100);
        let target_room_id = room_id.clone();
        let allowed_sender = config.allowed_sender.clone();
        let enable_matrix_logs = config.enable_matrix_logs;

        client.add_event_handler(move |event: OriginalSyncRoomMessageEvent, room: Room| {
            let tx = incoming_tx.clone();
            let target = target_room_id.clone();
            let allowed = allowed_sender.clone();
            let enable_logs = enable_matrix_logs;
            async move {
                handle_room_message(event, room, &target, &allowed, enable_logs, &tx).await;
            }
        });

        let sync_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = sync_client.sync(SyncSettings::default()).await {
                tracing::error!("Matrix sync loop exited with error: {e}");
            }
        });

        Ok(Self {
            client,
            room_id,
            incoming_rx,
            enable_matrix_logs,
        })
    }

    pub async fn send_message(&self, _recipient: &str, text: &str) -> KleviathanResult<()> {
        let room = self
            .client
            .get_room(&self.room_id)
            .ok_or_else(|| KleviathanError::Matrix("Configured room not found".into()))?;

        let content = RoomMessageEventContent::text_plain(text);

        tokio::time::timeout(std::time::Duration::from_secs(30), room.send(content))
            .await
            .map_err(|_| KleviathanError::Matrix("Send message timed out".into()))?
            .map_err(|e| KleviathanError::Matrix(format!("Failed to send message: {e}")))?;

        if self.enable_matrix_logs {
            tracing::info!(
                room_id = %self.room_id,
                message_text = %text,
                "Outgoing Matrix message sent"
            );
        }

        Ok(())
    }

    pub async fn recv_message(&mut self) -> Option<IncomingMessage> {
        self.incoming_rx.recv().await
    }
}

async fn build_client(config: &MatrixConfig) -> KleviathanResult<Client> {
    let store_path = store_path()?;
    std::fs::create_dir_all(&store_path)
        .map_err(|e| KleviathanError::Matrix(format!("Failed to create store directory: {e}")))?;

    Client::builder()
        .homeserver_url(&config.homeserver_url)
        .sqlite_store(&store_path, Some(&config.store_passphrase))
        .build()
        .await
        .map_err(|e| KleviathanError::Matrix(format!("Failed to build client: {e}")))
}

async fn login_or_restore(client: &Client, config: &MatrixConfig) -> KleviathanResult<()> {
    let session_path = session_file_path()?;

    if session_path.exists() {
        let data = std::fs::read_to_string(&session_path)
            .map_err(|e| KleviathanError::Matrix(format!("Failed to read session file: {e}")))?;
        let persisted: PersistedSession = serde_json::from_str(&data)
            .map_err(|e| KleviathanError::Matrix(format!("Failed to parse session file: {e}")))?;

        client
            .restore_session(persisted.session)
            .await
            .map_err(|e| KleviathanError::Matrix(format!("Failed to restore session: {e}")))?;

        tracing::info!("Restored existing Matrix session");
        return Ok(());
    }

    client
        .matrix_auth()
        .login_username(&config.username, &config.password)
        .initial_device_display_name("kleviathan")
        .send()
        .await
        .map_err(|e| KleviathanError::Matrix(format!("Login failed: {e}")))?;

    save_session(client).await?;
    tracing::info!("Logged in and saved new Matrix session");
    Ok(())
}

async fn save_session(client: &Client) -> KleviathanResult<()> {
    let session = client
        .matrix_auth()
        .session()
        .ok_or_else(|| KleviathanError::Matrix("No active session to save".into()))?;

    let persisted = PersistedSession {
        session,
        sync_token: None,
    };

    let session_path = session_file_path()?;
    let data = serde_json::to_string_pretty(&persisted)
        .map_err(|e| KleviathanError::Matrix(format!("Failed to serialize session: {e}")))?;

    std::fs::write(&session_path, data)
        .map_err(|e| KleviathanError::Matrix(format!("Failed to write session file: {e}")))?;

    Ok(())
}

async fn ensure_room_encrypted(client: &Client, room_id: &OwnedRoomId) -> KleviathanResult<()> {
    let room = client
        .get_room(room_id)
        .ok_or_else(|| KleviathanError::Matrix("Room not found for encryption check".into()))?;

    if !room
        .latest_encryption_state()
        .await
        .map_err(|e| {
            KleviathanError::Matrix(format!("Failed to check room encryption state: {e}"))
        })?
        .is_encrypted()
    {
        return Err(KleviathanError::Matrix(
            "Room does not have end-to-end encryption enabled. Kleviathan requires E2EE.".into(),
        ));
    }

    tracing::info!("Verified room has E2EE enabled");
    Ok(())
}

async fn resolve_dm_room(client: &Client, allowed_sender: &str) -> KleviathanResult<OwnedRoomId> {
    let user_id = <&UserId>::try_from(allowed_sender)
        .map_err(|e| KleviathanError::Matrix(format!("Invalid allowed_sender user ID: {e}")))?;

    if let Some(room) = client.get_dm_room(user_id) {
        tracing::info!("Found existing DM room with {}", allowed_sender);
        return Ok(room.room_id().to_owned());
    }

    tracing::info!("Creating new DM room with {}", allowed_sender);
    let room = client
        .create_dm(user_id)
        .await
        .map_err(|e| KleviathanError::Matrix(format!("Failed to create DM room: {e}")))?;

    Ok(room.room_id().to_owned())
}

fn is_valid_sender(sender: &str, allowed_sender: &str) -> bool {
    sender == allowed_sender
}

fn is_valid_message_text(body: &str) -> bool {
    !body.is_empty()
}

async fn handle_room_message(
    event: OriginalSyncRoomMessageEvent,
    room: Room,
    target_room_id: &OwnedRoomId,
    allowed_sender: &str,
    enable_matrix_logs: bool,
    tx: &mpsc::Sender<IncomingMessage>,
) {
    if room.room_id() != target_room_id {
        return;
    }
    if room.state() != RoomState::Joined {
        return;
    }
    if !is_valid_sender(event.sender.as_str(), allowed_sender) {
        tracing::warn!("Message from unauthorized sender: {}", event.sender);
        return;
    }

    let MessageType::Text(text_content) = event.content.msgtype else {
        return;
    };

    if !is_valid_message_text(&text_content.body) {
        return;
    }

    if enable_matrix_logs {
        tracing::info!(
            sender = %event.sender,
            message_text = %text_content.body,
            "Incoming Matrix message accepted"
        );
    }

    let _ = tx
        .send(IncomingMessage {
            sender: event.sender.to_string(),
            text: text_content.body,
        })
        .await;
}

fn config_dir() -> KleviathanResult<PathBuf> {
    let home = std::env::var("HOME")
        .map_err(|_| KleviathanError::Matrix("HOME environment variable not set".into()))?;
    Ok(PathBuf::from(home).join(".kleviathan"))
}

fn store_path() -> KleviathanResult<PathBuf> {
    Ok(config_dir()?.join("matrix_store"))
}

fn session_file_path() -> KleviathanResult<PathBuf> {
    Ok(config_dir()?.join("matrix_session.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use matrix_sdk::SessionMeta;
    use matrix_sdk::ruma::{device_id, user_id};

    fn make_test_session(sync_token: Option<String>) -> PersistedSession {
        let session = matrix_sdk::authentication::matrix::MatrixSession {
            meta: SessionMeta {
                user_id: user_id!("@bot:example.org").to_owned(),
                device_id: device_id!("DEVICEABC").to_owned(),
            },
            tokens: matrix_sdk::SessionTokens {
                access_token: "syt_token_123".to_owned(),
                refresh_token: None,
            },
        };
        PersistedSession {
            session,
            sync_token,
        }
    }

    #[test]
    fn persisted_session_roundtrips_through_json() {
        let original = make_test_session(None);
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: PersistedSession = serde_json::from_str(&serialized).unwrap();
        assert_eq!(
            deserialized.session.meta.user_id.as_str(),
            "@bot:example.org"
        );
        assert_eq!(deserialized.session.meta.device_id.as_str(), "DEVICEABC");
        assert!(deserialized.sync_token.is_none());
    }

    #[test]
    fn persisted_session_with_sync_token() {
        let original = make_test_session(Some("s12345_67890".to_owned()));
        let serialized = serde_json::to_string(&original).unwrap();
        let deserialized: PersistedSession = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.sync_token.as_deref(), Some("s12345_67890"));
    }

    #[test]
    fn e2ee_error_message_is_descriptive() {
        let err = KleviathanError::Matrix(
            "Room does not have end-to-end encryption enabled. Kleviathan requires E2EE.".into(),
        );
        assert!(err.to_string().contains("E2EE"));
    }

    #[test]
    fn valid_sender_is_accepted() {
        assert!(is_valid_sender("@user:matrix.org", "@user:matrix.org"));
    }

    #[test]
    fn wrong_sender_is_rejected() {
        assert!(!is_valid_sender("@hacker:evil.org", "@user:matrix.org"));
    }

    #[test]
    fn empty_sender_is_rejected() {
        assert!(!is_valid_sender("", "@user:matrix.org"));
    }

    #[test]
    fn non_empty_message_is_valid() {
        assert!(is_valid_message_text("hello"));
    }

    #[test]
    fn empty_message_is_invalid() {
        assert!(!is_valid_message_text(""));
    }

    #[test]
    fn whitespace_only_message_body_is_valid() {
        assert!(is_valid_message_text("   "));
    }

    #[test]
    fn incoming_message_preserves_fields() {
        let msg = IncomingMessage {
            sender: "@user:matrix.org".to_string(),
            text: "test message".to_string(),
        };
        assert_eq!(msg.sender, "@user:matrix.org");
        assert_eq!(msg.text, "test message");
    }

    #[test]
    fn config_deserializes_with_required_matrix_fields() {
        let json = r#"{
            "homeserver_url": "https://matrix.example.org",
            "username": "bot",
            "password": "pass",
            "allowed_sender": "@user:example.org",
            "store_passphrase": "secret",
            "enable_matrix_logs": false
        }"#;
        let config: MatrixConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.homeserver_url, "https://matrix.example.org");
        assert_eq!(config.allowed_sender, "@user:example.org");
        assert!(!config.enable_matrix_logs);
    }

    #[test]
    fn valid_user_id_parses() {
        assert!(<&UserId>::try_from("@user:matrix.org").is_ok());
    }

    #[test]
    fn invalid_user_id_fails() {
        assert!(<&UserId>::try_from("not-a-user-id").is_err());
    }
}
