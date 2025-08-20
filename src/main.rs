use clap::Parser;
use reqwest;
use std::fs::File;
use std::io::{stdin, Write};
use std::sync::{Arc, Mutex};
use steamworks::{Client, TicketForWebApiResponse};


#[derive(Parser)]
#[command(name = "steam-auth")]
#[command(about = "Steam authentication ticket generator")]
#[command(
    long_about = "Generates Steam authentication tickets for web API usage. Can either POST the ticket to a URL with email credentials or save it to a local file."
)]
struct Args {
    /// URL endpoint to POST the authentication ticket to
    ///
    /// When provided, must be used together with `--email`. The ticket will be sent
    /// as a POST request to this URL with `email` and `authTicket` query parameters.
    #[arg(
        long,
        short = 'u',
        group = "post_mode",
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
        group = "post_mode",
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
        conflicts_with = "post_mode",
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
        conflicts_with = "post_mode",
        help = "Exit immediately after writing ticket file"
    )]
    exit: bool,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();


    // Initialize Steam client
    let client = match Client::init() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("Failed to initialize Steam client: {:?}", e);
            return;
        }
    };

    println!("Steam client initialized successfully!");

    // Shared state to store the ticket when callback fires
    let ticket_data = Arc::new(Mutex::new(None::<Vec<u8>>));
    let ticket_data_clone = ticket_data.clone();

    // Register callback for ticket response
    let _cb = client.register_callback(move |response: TicketForWebApiResponse| {
        println!("Got ticket response: {:?}", response);

        match response.result {
            Ok(()) => {
                println!("Ticket generated successfully, {} bytes", response.ticket.len());
                *ticket_data_clone.lock().unwrap() = Some(response.ticket.clone());
            }
            Err(e) => {
                eprintln!("Failed to generate ticket: {:?}", e);
            }
        }
    });

    // Get user and check login status
    let user = client.user();
    if !user.logged_on() {
        eprintln!("User is not logged into Steam");
        return;
    }

    println!("Steam ID: {}", user.steam_id().raw());

    // Request auth ticket for web API
    let auth_ticket_handle = user.authentication_session_ticket_for_webapi("BitCraftApiServer");
    println!("Auth ticket handle: {:?}", auth_ticket_handle);
    println!("Waiting for ticket response...");

    // Wait for callback to receive actual ticket data
    let mut ticket_received = false;
    let mut attempts = 0;
    while !ticket_received && attempts < 100 {
        client.run_callbacks();

        if let Some(ticket) = ticket_data.lock().unwrap().as_ref() {
            println!("Received ticket with {} bytes", ticket.len());

            // Either POST to URL or write to file
            if let (Some(url), Some(email)) = (&args.url, &args.email) {
                println!("Attempting to post ticket to URL: {} with email: {}", url, email);
                match post_ticket_to_url(url, email, ticket).await {
                    Ok(_) => {
                        println!("Successfully authenticated!");
                        std::process::exit(0);
                    } // Succeed silently on 200 OK
                    Err(e) => {
                        eprintln!("Error posting ticket: {}", e);
                        panic!("Failed to post auth ticket");
                    }
                }
            } else {
                match write_ticket_to_file(ticket, &args.output_file) {
                    Ok(_) => println!("Ticket written to {}", args.output_file),
                    Err(e) => eprintln!("Failed to write ticket to file: {:?}", e),
                }
            }
            ticket_received = true;
        } else {
            attempts += 1;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    if !ticket_received {
        eprintln!("Timeout waiting for ticket response");
    }

    // Keep Steam client alive until Enter is pressed
    println!("Session held open. Press Enter to exit...");

    std::thread::spawn(|| {
        let mut input = String::new();
        stdin().read_line(&mut input).unwrap();
        std::process::exit(0);
    });

    // Keep running callbacks forever until user presses Enter
    // this is to allow user to post the ticket manually
    loop {
        client.run_callbacks();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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
