use byteorder::{LittleEndian, WriteBytesExt};
use crc32fast::Hasher;
use rand::RngCore;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::io::{Cursor, Write};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use steam_vent::{Connection, ConnectionTrait, EResult};
use steam_vent_proto::steammessages_base::CMsgAuthTicket;
use steam_vent_proto::steammessages_clientserver::{CMsgClientAuthList, CMsgClientAuthListAck, CMsgClientGetAppOwnershipTicket, CMsgClientGetAppOwnershipTicketResponse};
// Adjust imports as needed

#[derive(Debug, Clone, Copy)]
pub enum TicketType {
    AuthSession = 2,
    WebApiTicket = 5,
}

#[derive(Debug)]
pub struct TicketInfo {
    pub appid: u32,
    pub token: Vec<u8>,
    pub ticket_crc: u32,
}

#[derive(Debug, Clone)]
struct AuthTicket {
    gameid: u32,
    ticket: Vec<u8>,
    ticket_crc: u32,
    server_secret: Option<Vec<u8>>,
}

const WEB_API_TICKET_SIZE: usize = 2560;
static SEQUENCE: AtomicU32 = AtomicU32::new(0);

/// Steam authentication ticket handler
pub struct SteamAuthTicket {
    game_connect_tokens: VecDeque<Vec<u8>>,
    tickets_by_game: HashMap<u32, Vec<AuthTicket>>,
}

impl SteamAuthTicket {
    pub fn new() -> Self {
        Self {
            game_connect_tokens: VecDeque::new(),
            tickets_by_game: HashMap::new(),
        }
    }

    pub async fn get_auth_ticket_for_web_api(
        &mut self,
        connection: &Connection,
        appid: u32,
        identity: String,
    ) -> Result<TicketInfo, Box<dyn Error>> {
        self.get_auth_session_ticket_internal(connection, appid, TicketType::WebApiTicket, Some(identity)).await
    }

    pub async fn get_auth_session_ticket(
        &mut self,
        connection: &Connection,
        appid: u32,
    ) -> Result<TicketInfo, Box<dyn Error>> {
        self.get_auth_session_ticket_internal(connection, appid, TicketType::AuthSession, None).await
    }

    async fn get_auth_session_ticket_internal(
        &mut self,
        connection: &Connection,
        appid: u32,
        ticket_type: TicketType,
        identity: Option<String>,
    ) -> Result<TicketInfo, Box<dyn Error>> {
        let app_ticket = get_app_ownership_ticket(connection, appid).await?;

        let token = self.game_connect_tokens.pop_front()
            .ok_or("There's no available game connect tokens left.")?;

        let auth_ticket = build_auth_ticket(&token, ticket_type);

        // Steam add the 'str:' prefix to the identity string itself and appends a null terminator
        let server_secret = match identity {
            Some(id) if !id.is_empty() => Some(format!("str:{}\0", id).into_bytes()),
            _ => None,
        };

        let crc = self.verify_ticket(connection, appid, &auth_ticket, server_secret).await?;

        let combined_ticket = combine_tickets(
            &auth_ticket,
            &app_ticket,
            matches!(ticket_type, TicketType::WebApiTicket),
        );

        Ok(TicketInfo {
            appid,
            token: combined_ticket,
            ticket_crc: crc,
        })
    }

    async fn verify_ticket(
        &mut self,
        connection: &Connection,
        appid: u32,
        auth_ticket: &[u8],
        server_secret: Option<Vec<u8>>,
    ) -> Result<u32, Box<dyn Error>> {
        // Calculate CRC32
        let mut hasher = Hasher::new();
        hasher.update(auth_ticket);
        let crc = hasher.finalize();

        // Add ticket to game list
        let tickets = self.tickets_by_game.entry(appid).or_insert_with(Vec::new);
        tickets.push(AuthTicket {
            gameid: appid,
            ticket: auth_ticket.to_vec(),
            ticket_crc: crc,
            server_secret,
        });

        self.send_tickets(connection).await?;
        Ok(crc)
    }

    async fn send_tickets(&self, connection: &Connection) -> Result<(), Box<dyn Error>> {
        let app_ids: Vec<u32> = self.tickets_by_game.keys().cloned().collect();
        let all_tickets: Vec<_> = self.tickets_by_game
            .values()
            .flat_map(|tickets| {
                tickets.iter().map(|t| CMsgAuthTicket {
                    gameid: Some(t.gameid as u64),
                    ticket: Some(t.ticket.clone()),
                    ticket_crc: Some(t.ticket_crc),
                    server_secret: t.server_secret.clone(),
                    ..Default::default()
                })
            })
            .collect();

        let msg = CMsgClientAuthList {
            tokens_left: Some(self.game_connect_tokens.len() as u32),
            app_ids,
            tickets: all_tickets,
            ..Default::default()
        };

        // Send and wait for acknowledgment
        let _response: CMsgClientAuthListAck = connection.job(msg).await?;
        Ok(())
    }

    pub fn cancel_auth_ticket(&mut self, ticket_info: &TicketInfo) {
        if let Some(tickets) = self.tickets_by_game.get_mut(&ticket_info.appid) {
            tickets.retain(|t| t.ticket_crc != ticket_info.ticket_crc);
        }
    }


    pub fn handle_game_connect_tokens(&mut self, tokens: Vec<Vec<u8>>, max_tokens_to_keep: u32) {
        println!("Received {} game connect tokens, keeping max {}", tokens.len(), max_tokens_to_keep);

        // Add new tokens
        for token in tokens {
            self.game_connect_tokens.push_back(token);
        }
        
        // Keep only the required amount, discard old entries
        while self.game_connect_tokens.len() > max_tokens_to_keep as usize {
            self.game_connect_tokens.pop_front();
        }
    }

    /// Call this on logout to clear all tokens and tickets
    pub fn handle_log_off(&mut self) {
        self.game_connect_tokens.clear();
        self.tickets_by_game.clear();
    }

    pub fn tokens_available(&self) -> usize {
        self.game_connect_tokens.len()
    }
}

impl Default for SteamAuthTicket {
    fn default() -> Self {
        Self::new()
    }
}



pub async fn get_app_ownership_ticket(
    connection: &Connection,
    appid: u32,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let msg = CMsgClientGetAppOwnershipTicket {
        app_id: Some(appid),
        ..Default::default()
    };

    let response: CMsgClientGetAppOwnershipTicketResponse = connection.job(msg).await?;
    
    match EResult::try_from(response.eresult().cast_signed()).unwrap() {
        EResult::OK => {
            Ok(response.ticket().to_vec())
        }
        result => {
            Err(format!(
                "Failed to obtain app ownership ticket. Result: {:?}. The user may not own the game or there was an error.",
                result
            ).into())
        }
    }
}

fn build_auth_ticket(game_connect_token: &[u8], ticket_type: TicketType) -> Vec<u8> {
    const SESSION_SIZE: usize = 
        4 + // unknown, always 1
        4 + // TicketType, 2 or 5
        4 + // public IP v4, optional
        4 + // private IP v4, optional
        4 + // timestamp
        4;  // sequence

    let total_size = game_connect_token.len() + 4 + 4 + SESSION_SIZE;
    let mut ticket = Vec::with_capacity(total_size);
    let mut cursor = Cursor::new(&mut ticket);

    // Write game connect token length and token
    cursor.write_u32::<LittleEndian>(game_connect_token.len() as u32).unwrap();
    cursor.write_all(game_connect_token).unwrap();

    // Write session size
    cursor.write_u32::<LittleEndian>(SESSION_SIZE as u32).unwrap();

    // Write session data
    cursor.write_u32::<LittleEndian>(1).unwrap(); // unknown, always 1
    cursor.write_u32::<LittleEndian>(ticket_type as u32).unwrap();

    // Write 8 random bytes (public/private IP placeholders)
    let mut random_bytes = [0u8; 8];
    rand::rng().fill_bytes(&mut random_bytes);
    cursor.write_all(&random_bytes).unwrap();

    // Write timestamp (using current time as approximation of Stopwatch.GetTimestamp())
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    cursor.write_u32::<LittleEndian>(timestamp).unwrap();

    // Write sequence number
    let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
    cursor.write_u32::<LittleEndian>(sequence).unwrap();

    ticket
}

fn combine_tickets(auth_ticket: &[u8], app_ticket: &[u8], pad_to_web_api_size: bool) -> Vec<u8> {
    let raw_size = auth_ticket.len() + 4 + app_ticket.len();
    let target_size = if pad_to_web_api_size {
        raw_size.max(WEB_API_TICKET_SIZE)
    } else {
        raw_size
    };

    let mut token = vec![0u8; target_size];
    
    // Copy auth ticket
    token[0..auth_ticket.len()].copy_from_slice(auth_ticket);
    
    // Write app ticket length
    let len_bytes = (app_ticket.len() as u32).to_le_bytes();
    token[auth_ticket.len()..auth_ticket.len() + 4].copy_from_slice(&len_bytes);
    
    // Copy app ticket
    let app_start = auth_ticket.len() + 4;
    token[app_start..app_start + app_ticket.len()].copy_from_slice(app_ticket);

    // Fill remaining space with random data for WebAPI tickets
    if pad_to_web_api_size && raw_size < target_size {
        rand::rng().fill_bytes(&mut token[raw_size..]);
    }

    token
}