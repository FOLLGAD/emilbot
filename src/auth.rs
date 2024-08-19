use futures_util::StreamExt;
use matrix_sdk::{
    encryption::verification::{
        format_emojis, Emoji, SasState, SasVerification, Verification, VerificationRequest,
        VerificationRequestState,
    },
    matrix_auth::MatrixSession,
    ruma::{
        events::{
            key::verification::request::ToDeviceKeyVerificationRequestEvent,
            room::message::{MessageType, OriginalSyncRoomMessageEvent},
        },
        UserId,
    },
    Client,
};
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
};
use tokio::fs;

/// Persist the sync token for a future session.
/// Note that this is needed only when using `sync_once`. Other sync methods get
/// the sync token from the store.
pub async fn persist_sync_token(session_file: &Path, sync_token: String) -> anyhow::Result<()> {
    let serialized_session = fs::read_to_string(session_file).await?;
    let mut full_session: FullSession = serde_json::from_str(&serialized_session)?;

    full_session.sync_token = Some(sync_token);
    let serialized_session = serde_json::to_string(&full_session)?;
    fs::write(session_file, serialized_session).await?;

    Ok(())
}

/// Restore a previous session.
pub async fn restore_session(session_file: &Path) -> anyhow::Result<(Client, Option<String>)> {
    println!(
        "Previous session found in '{}'",
        session_file.to_string_lossy()
    );

    // The session was serialized as JSON in a file.
    let serialized_session = fs::read_to_string(session_file).await?;
    let FullSession {
        client_session,
        user_session,
        sync_token,
    } = serde_json::from_str(&serialized_session)?;

    // Build the client with the previous settings from the session.
    let client = Client::builder()
        .homeserver_url(client_session.homeserver)
        .sqlite_store(client_session.db_path, Some(&client_session.passphrase))
        .build()
        .await?;

    println!("Restoring session for {}…", user_session.meta.user_id);

    // Restore the Matrix user session.
    client.restore_session(user_session).await?;

    Ok((client, sync_token))
}

/// Login with a new device.
pub async fn login(data_dir: &Path, session_file: &Path) -> anyhow::Result<Client> {
    println!("No previous session found, logging in…");

    let (client, client_session) = build_client(data_dir).await?;
    let matrix_auth = client.matrix_auth();

    loop {
        print!("\nUsername: ");
        io::stdout().flush().expect("Unable to write to stdout");
        let mut username = String::new();
        io::stdin()
            .read_line(&mut username)
            .expect("Unable to read user input");
        username = username.trim().to_owned();

        print!("Password: ");
        io::stdout().flush().expect("Unable to write to stdout");
        let mut password = String::new();
        io::stdin()
            .read_line(&mut password)
            .expect("Unable to read user input");
        password = password.trim().to_owned();

        match matrix_auth
            .login_username(&username, &password)
            .initial_device_display_name("oxybot client")
            .await
        {
            Ok(_) => {
                println!("Logged in as {username}");
                break;
            }
            Err(error) => {
                println!("Error logging in: {error}");
                println!("Please try again\n");
            }
        }
    }

    // Persist the session to reuse it later.
    // This is not very secure, for simplicity. If the system provides a way of
    // storing secrets securely, it should be used instead.
    // Note that we could also build the user session from the login response.
    let user_session = matrix_auth
        .session()
        .expect("A logged-in client should have a session");
    let serialized_session = serde_json::to_string(&FullSession {
        client_session,
        user_session,
        sync_token: None,
    })?;
    fs::write(session_file, serialized_session).await?;

    println!("Session persisted in {}", session_file.to_string_lossy());

    Ok(client)
}

/// Build a new client.
async fn build_client(data_dir: &Path) -> anyhow::Result<(Client, ClientSession)> {
    let mut rng = thread_rng();

    // Generating a subfolder for the database is not mandatory, but it is useful if
    // you allow several clients to run at the same time. Each one must have a
    // separate database, which is a different folder with the SQLite store.
    let db_subfolder: String = (&mut rng)
        .sample_iter(Alphanumeric)
        .take(7)
        .map(char::from)
        .collect();
    let db_path = data_dir.join(db_subfolder);

    // Generate a random passphrase.
    let passphrase: String = (&mut rng)
        .sample_iter(Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();

    // We create a loop here so the user can retry if an error happens.
    loop {
        let mut homeserver = String::new();

        print!("Homeserver URL: ");
        io::stdout().flush().expect("Unable to write to stdout");
        io::stdin()
            .read_line(&mut homeserver)
            .expect("Unable to read user input");

        println!("\nChecking homeserver…");

        match Client::builder()
            .homeserver_url(&homeserver)
            // We use the SQLite store, which is enabled by default. This is the crucial part to
            // persist the encryption setup.
            // Note that other store backends are available and you can even implement your own.
            .sqlite_store(&db_path, Some(&passphrase))
            .build()
            .await
        {
            Ok(client) => {
                return Ok((
                    client,
                    ClientSession {
                        homeserver,
                        db_path,
                        passphrase,
                    },
                ))
            }
            Err(error) => match &error {
                matrix_sdk::ClientBuildError::AutoDiscovery(_)
                | matrix_sdk::ClientBuildError::Url(_)
                | matrix_sdk::ClientBuildError::Http(_) => {
                    println!("Error checking the homeserver: {error}");
                    println!("Please try again\n");
                }
                _ => {
                    // Forward other errors, it's unlikely we can retry with a different outcome.
                    return Err(error.into());
                }
            },
        }
    }
}

async fn wait_for_confirmation(sas: SasVerification, emoji: [Emoji; 7]) {
    println!("\nDo the emojis match: \n{}", format_emojis(emoji));
    print!("Confirm with `yes` or cancel with `no`: ");
    std::io::stdout()
        .flush()
        .expect("We should be able to flush stdout");

    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .expect("error: unable to read user input");

    match input.trim().to_lowercase().as_ref() {
        "yes" | "true" | "ok" => sas.confirm().await.unwrap(),
        _ => sas.cancel().await.unwrap(),
    }
}

async fn print_devices(user_id: &UserId, client: &Client) {
    println!("Devices of user {user_id}");

    for device in client
        .encryption()
        .get_user_devices(user_id)
        .await
        .unwrap()
        .devices()
    {
        if device.device_id()
            == client
                .device_id()
                .expect("We should be logged in now and know our device id")
        {
            continue;
        }

        println!(
            "   {:<10} {:<30} {:<}",
            device.device_id(),
            device.display_name().unwrap_or("-"),
            if device.is_verified() { "✅" } else { "❌" }
        );
    }
}

async fn sas_verification_handler(client: Client, sas: SasVerification) {
    println!(
        "Starting verification with {} {}",
        &sas.other_device().user_id(),
        &sas.other_device().device_id()
    );
    print_devices(sas.other_device().user_id(), &client).await;
    sas.accept().await.unwrap();

    let mut stream = sas.changes();

    while let Some(state) = stream.next().await {
        match state {
            SasState::KeysExchanged {
                emojis,
                decimals: _,
            } => {
                tokio::spawn(wait_for_confirmation(
                    sas.clone(),
                    emojis
                        .expect("We only support verifications using emojis")
                        .emojis,
                ));
            }
            SasState::Done { .. } => {
                let device = sas.other_device();

                println!(
                    "Successfully verified device {} {} {:?}",
                    device.user_id(),
                    device.device_id(),
                    device.local_trust_state()
                );

                print_devices(sas.other_device().user_id(), &client).await;

                break;
            }
            SasState::Cancelled(cancel_info) => {
                println!(
                    "The verification has been cancelled, reason: {}",
                    cancel_info.reason()
                );

                break;
            }
            SasState::Started { .. } | SasState::Accepted { .. } | SasState::Confirmed => (),
        }
    }
}
async fn request_verification_handler(client: Client, request: VerificationRequest) {
    println!(
        "Accepting verification request from {}",
        request.other_user_id(),
    );
    request
        .accept()
        .await
        .expect("Can't accept verification request");

    let mut stream = request.changes();

    while let Some(state) = stream.next().await {
        match state {
            VerificationRequestState::Created { .. }
            | VerificationRequestState::Requested { .. }
            | VerificationRequestState::Ready { .. } => (),
            VerificationRequestState::Transitioned { verification } => {
                // We only support SAS verification.
                if let Verification::SasV1(s) = verification {
                    tokio::spawn(sas_verification_handler(client, s));
                    break;
                }
            }
            VerificationRequestState::Done | VerificationRequestState::Cancelled(_) => break,
        }
    }
}

pub async fn setup_verification(client: &Client) {
    println!("Setting up verification…");
    // EMOJI VERIFICATION HERE
    client.add_event_handler(
        |ev: ToDeviceKeyVerificationRequestEvent, client: Client| async move {
            let request = client
                .encryption()
                .get_verification_request(&ev.sender, &ev.content.transaction_id)
                .await
                .expect("Request object wasn't created");

            tokio::spawn(request_verification_handler(client, request));
        },
    );

    client.add_event_handler(
        |ev: OriginalSyncRoomMessageEvent, client: Client| async move {
            if let MessageType::VerificationRequest(_) = &ev.content.msgtype {
                let request = client
                    .encryption()
                    .get_verification_request(&ev.sender, &ev.event_id)
                    .await
                    .expect("Request object wasn't created");

                tokio::spawn(request_verification_handler(client, request));
            }
        },
    );
}

/// The data needed to re-build a client.
#[derive(Debug, Serialize, Deserialize)]
struct ClientSession {
    /// The URL of the homeserver of the user.
    homeserver: String,

    /// The path of the database.
    db_path: PathBuf,

    /// The passphrase of the database.
    passphrase: String,
}

/// The full session to persist.
#[derive(Debug, Serialize, Deserialize)]
struct FullSession {
    /// The data to re-build the client.
    client_session: ClientSession,

    /// The Matrix user session.
    user_session: MatrixSession,

    /// The latest sync token.
    ///
    /// It is only needed to persist it when using `Client::sync_once()` and we
    /// want to make our syncs faster by not receiving all the initial sync
    /// again.
    #[serde(skip_serializing_if = "Option::is_none")]
    sync_token: Option<String>,
}
