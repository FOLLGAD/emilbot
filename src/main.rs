use log::info;
use matrix_sdk::{
    config::SyncSettings,
    ruma::{
        api::client::filter::FilterDefinition,
        events::room::message::{
            MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
        },
    },
    Client, Error, LoopCtrl, Room, RoomState,
};
use std::path::Path;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, layer::SubscriberExt, EnvFilter};

mod auth;

fn init_custom_logger() {
    let crate_name = "oxybot";

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("{}=info", crate_name)));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(env_filter)
        .init();
}

const CLIENT_NAME: &str = "oxybot";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_custom_logger();

    info!("Starting {}", CLIENT_NAME);

    // Stores in ~/Library/Application Support/oxybot
    let data_dir = dirs::data_dir()
        .expect("no data_dir directory found")
        .join(CLIENT_NAME);
    // The file where the session is persisted.
    let session_file = data_dir.join("session");

    let (client, sync_token) = if session_file.exists() {
        auth::restore_session(&session_file).await?
    } else {
        (auth::login(&data_dir, &session_file).await?, None)
    };

    auth::setup_verification(&client).await;

    // Wait for the first sync response
    println!("Wait for the first sync");

    client.sync_once(SyncSettings::default()).await?;

    sync(client, sync_token, &session_file)
        .await
        .map_err(Into::into)
}

/// Setup the client to listen to new messages.
async fn sync(
    client: Client,
    initial_sync_token: Option<String>,
    session_file: &Path,
) -> anyhow::Result<()> {
    // https://spec.matrix.org/v1.6/client-server-api/#lazy-loading-room-members
    let filter = FilterDefinition::with_lazy_loading();
    let mut sync_settings = SyncSettings::default().filter(filter.into());

    let ignore_past_messages = true;
    if ignore_past_messages {
        println!("Launching a first sync to ignore past messages…");

        // This is not necessary when not using `sync_once`. The other sync methods get
        // the sync token from the store.
        if let Some(sync_token) = initial_sync_token {
            sync_settings = sync_settings.token(sync_token);
        }

        loop {
            match client.sync_once(sync_settings.clone()).await {
                Ok(response) => {
                    // This is the last time we need to provide this token, the sync method after
                    // will handle it on its own.
                    sync_settings = sync_settings.token(response.next_batch.clone());
                    auth::persist_sync_token(session_file, response.next_batch).await?;
                    break;
                }
                Err(error) => {
                    println!("An error occurred during initial sync: {error}");
                    println!("Trying again…");
                }
            }
        }
    }

    println!("The client is ready! Listening to new messages…");

    // Now that we've synced, let's attach a handler for incoming room messages.
    client.add_event_handler(on_room_message);

    // This loops until we kill the program or an error happens.
    client
        .sync_with_result_callback(sync_settings, |sync_result| async move {
            let response = sync_result?;

            // We persist the token each time to be able to restore our session
            auth::persist_sync_token(session_file, response.next_batch)
                .await
                .map_err(|err| Error::UnknownError(err.into()))?;

            Ok(LoopCtrl::Continue)
        })
        .await?;

    Ok(())
}

/// Handle room messages.
async fn on_room_message(event: OriginalSyncRoomMessageEvent, room: Room) {
    // We only want to log text messages in joined rooms.
    if room.state() != RoomState::Joined {
        return;
    }

    let MessageType::Text(text_content) = &event.content.msgtype else {
        return;
    };

    let room_name = match room.display_name().await {
        Ok(room_name) => room_name.to_string(),
        Err(error) => {
            println!("Error getting room display name: {error}");
            // Let's fallback to the room ID.
            room.room_id().to_string()
        }
    };

    let client = room.client();
    let user_id = client.user_id().unwrap();

    if text_content.body.starts_with("!oxy") && event.sender != user_id {
        let user = client.get_profile(user_id).await.unwrap();
        let display_name = user.displayname.unwrap_or("Stranger".to_string());

        let message =
            RoomMessageEventContent::text_plain("Well hello there ".to_string() + &display_name);
        room.send(message).await.unwrap();
    }

    info!("[{room_name}] {}: {}", event.sender, text_content.body)
}
