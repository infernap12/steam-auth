mod steam_auth_rust;

use base64::Engine;
use clap::Parser;
use futures_util::StreamExt;
use reqwest;
use std::error::Error;
use std::fs::File;
use std::io::{stdin, Read, Write};
use steam_vent::auth::{AuthConfirmationHandler, ConsoleAuthConfirmationHandler, DeviceConfirmationHandler, FileGuardDataStore, GuardDataStore};
use steam_vent::{Connection, ConnectionTrait, ServerList};
use steam_vent_proto::steammessages_clientserver::{CMsgClientGameConnectTokens, CMsgClientGamesPlayed, CMsgClientGetAppOwnershipTicket, CMsgClientGetAppOwnershipTicketResponse};
use tokio::io::AsyncWriteExt;

use crate::steam_auth_rust::SteamAuthTicket;
use byteorder::WriteBytesExt;
use rand::RngCore;
use steam_vent_proto::enums_clientserver::EMsg;
use steam_vent_proto::steammessages_clientserver::cmsg_client_games_played::GamePlayed;

#[derive(Parser)]
#[command(name = "steam-auth")]
#[command(about = "Steam authentication ticket generator")]
#[command(
    long_about = "Generates Steam authentication tickets for web API usage. Can either POST the ticket to a URL with email credentials or save it to a local file."
)]
struct Args {
    /// Steam account username
    #[arg(
        long,
        short = 'a',
        required = true,
        help = "Steam account username"
    )]
    account: String,

    /// Steam account password
    #[arg(
        long,
        short = 'p',
        required = true,
        help = "Steam account password"
    )]
    password: String,

    /// Service identity to embed in ticket
    #[arg(
        long,
        short = 's',
        required = true,
        help = "Service identity to embed in ticket"
    )]
    service_identity: String,

    /// URL endpoint to POST the authentication ticket to
    ///
    /// When provided, must be used together with `--email`. The ticket will be sent
    /// as a POST request to this URL with `email` and `authTicket` query parameters.
    #[arg(
        long,
        short = 'u',
        group = "action",
        requires = "email",
        help = "URL to POST authentication ticket to"
    )]
    url: Option<String>,

    /// Email address to send with the authentication ticket
    ///
    /// Required when using --url. The email will be sent as a query parameter
    /// along with the authentication ticket when POSTing to the specified URL.
    #[arg(
        long,
        short = 'e',
        requires = "url",
        help = "Email to send with auth ticket"
    )]
    email: Option<String>,

    /// Output file path to save the authentication ticket
    ///
    /// When neither `--url` nor `--email` are provided, the authentication ticket
    /// will be saved to this file as a hexadecimal string. Defaults to `auth_ticket.txt`
    /// in the current directory.
    #[arg(
        long,
        short = 'o',
        default_value = "auth_ticket.txt",
        group = "action",
        help = "Output file for auth ticket"
    )]
    output_file: String,

    /// Exit immediately after writing ticket to file
    ///
    /// When saving to output file, exit the program immediately after writing
    /// the ticket instead of keeping the Steam client running and waiting for Enter.
    #[arg(
        long,
        short = 'x',
        requires = "output_file",
        help = "Exit immediately after writing ticket file"
    )]
    exit: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    // Initialize guard data and connection
    let guard_data = FileGuardDataStore::user_cache();


    println!("Connecting to Steam...");

    let handler = ConsoleAuthConfirmationHandler::default().or(DeviceConfirmationHandler);
    let server_list = ServerList::discover().await?;

    let connection = Connection::login(
        &server_list,
        &args.account,
        &args.password,
        guard_data,
        handler,
    ).await?;


    println!("Successfully connected to Steam!");





    // Set up token message handler
    // let mut tokens_messages = connection.on::<CMsgClientGameConnectTokens>();

    // Read app ID from file
    let app_id = std::fs::read_to_string("steam_appid.txt")
        .map_err(|e| format!("Failed to read steam_appid.txt: {}", e))?
        .trim()
        .parse::<u32>()
        .map_err(|e| format!("Invalid app ID in steam_appid.txt: {}", e))?;

    println!("Using app ID: {}", app_id);

    // let gc = GameCoordinator::new(&connection, app_id).await?;

    // Get app ownership ticket
    let msg = CMsgClientGetAppOwnershipTicket {
        app_id: Some(app_id),
        ..Default::default()
    };

    let response: CMsgClientGetAppOwnershipTicketResponse = connection.job(msg).await?;
    dbg!(response.ticket);

    let games_msg = CMsgClientGamesPlayed {
        games_played: vec![GamePlayed {
            game_id: Some(app_id as u64), // CS:GO for testing
            ..Default::default()
        }],
        ..Default::default()
    };
    connection.send_with_kind(games_msg, EMsg::k_EMsgClientGamesPlayed).await?;

    let mut tokens_messages = connection.on::<CMsgClientGameConnectTokens>();
    // Initialize auth handler and process game connect tokens
    let mut auth_handler = SteamAuthTicket::new();
    if let Some(Ok(tokens_message)) = tokens_messages.next().await {
        println!("Received {} game connect tokens", tokens_message.tokens.len());
        auth_handler.handle_game_connect_tokens(
            tokens_message.tokens.clone(),
            tokens_message.max_tokens_to_keep()
        );
    }

    println!("Generating web API authentication ticket...");

    // Generate the web API ticket using the service identity from args
    let ticket = auth_handler
        .get_auth_ticket_for_web_api(&connection, app_id, args.service_identity.clone())
        .await?;

    println!("Successfully generated authentication ticket ({} bytes)", ticket.token.len());

    // Handle the ticket based on command line arguments
    if let (Some(url), Some(email)) = (&args.url, &args.email) {
        // POST to URL
        println!("Posting authentication ticket to: {}", url);
        match post_ticket_to_url(url, email, &ticket.token).await {
            Ok(_) => {
                println!("Successfully authenticated with server!");
                return Ok(());
            }
            Err(e) => {
                eprintln!("Error posting ticket to URL: {}", e);
                return Err(e);
            }
        }
    } else {
        // Write to file
        println!("Writing authentication ticket to: {}", args.output_file);
        match write_ticket_to_file(&ticket.token, &args.output_file) {
            Ok(_) => {
                println!("Ticket written to {}", args.output_file);
            }
            Err(e) => {
                eprintln!("Failed to write ticket to file: {}", e);
                return Err(e);
            }
        }

        // Exit immediately if requested, otherwise wait for user input
        if args.exit {
            println!("Exiting immediately as requested.");
            return Ok(());
        } else {
            println!("Authentication ticket generated. Press Enter to exit...");
            let mut input = String::new();
            stdin().read_line(&mut input)?;
        }
    }

    Ok(())
}

async fn post_ticket_to_url(url: &str, email: &str, ticket: &[u8]) -> Result<(), Box<dyn std::error::Error>> {
    let hex_ticket = ticket.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();

    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .query(&[("email", email), ("authTicket", &hex_ticket)])
        .send()
        .await?;

    if response.status() == 200 {
        Ok(())
    } else {
        Err(format!("Server returned status: {}", response.status()).into())
    }
}

fn write_ticket_to_file(ticket: &[u8], filename: &str) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = File::create(filename)?;

    // Write ticket as hex string
    let hex_ticket = ticket.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();

    writeln!(file, "{}", hex_ticket)?;

    Ok(())
}