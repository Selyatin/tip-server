#[macro_use] extern crate log;
#[macro_use] extern crate lazy_static;

use tokio::{
    net::{TcpListener, TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}},
    io::{AsyncWriteExt, Sink}
};
use rand::Rng;
use dashmap::DashMap;

lazy_static! {
    static ref SESSIONS: DashMap<u16, Vec<(u8, OwnedWriteHalf)>> = DashMap::default();
}

#[tokio::main]
async fn main() -> anyhow::Result<()>{
    env_logger::init();

    let listener = TcpListener::bind("0.0.0.0:8080").await?;

    debug!("Listener initialised");

    loop {
        let (socket, _) = listener.accept().await?;

        debug!("Received New Client");

        let (read_half, write_half) = socket.into_split();

        tokio::spawn(async move {
            if let Err(err) = client_interaction(read_half, write_half).await {
                error!("{}", err);
            }
        });
    }
}

async fn client_interaction(
    read_half: OwnedReadHalf, 
    write_half: OwnedWriteHalf
) -> anyhow::Result<()> {
    let mut id: u8 = 0;
    let mut session_token: Option<u16> = None;

    debug!("Client Connected");

    loop {
        read_half.readable().await?;

        debug!("Readable");

        let mut buffer = vec![0u8; 11];

        if let Ok(size) = read_half.try_read(&mut buffer) {
            if size < 1 {
                debug!("Packet empty");
                break;
            }
        }

        buffer.retain(|b| *b != 0);

        debug!("Readed");

        if &buffer[0..6] == b"Create" {
            debug!("Creating New Session");
            loop {
                let token = rand::thread_rng().gen_range(0..65535);
                if SESSIONS.get(&token).is_some() {
                    continue;
                }
                write_half.writable().await?;
                write_half.try_write(&token.to_be_bytes())?;
                SESSIONS.insert(token, vec![(id, write_half)]);
                session_token = Some(token);
                debug!("New Session Created");
                break;
            }
            break;
        } else if &buffer[0..4] == b"Join" {
            let first_byte = match buffer.get(4) {
                Some(first_byte) => *first_byte,
                None => break
            };
            let second_byte = match buffer.get(5) {
                Some(second_byte) => *second_byte,
                None => break
            };

            session_token = Some(u16::from_be_bytes([first_byte, second_byte]));

            if let Some(mut clients) = SESSIONS.get_mut(&session_token.unwrap()){
                let vec = clients.value_mut();
                id = vec.len() as u8;
                // Notify the users who are in the session that a new user joined
                for (_, write_half) in vec.iter() {
                    write_half.writable().await?;
                    write_half.try_write(&[b'J', b'o', b'i', b'n', b'e', b'd', id])?;
                }
                vec.push((id, write_half));
            }
            break;
        }
    }

    let session_token = match session_token {
        Some(token) => token,
        None => return Ok(())
    };

    debug!("Session Token {}", session_token);

    let mut buffer = [0u8; 1];

    loop {
        if read_half.readable().await.is_err() {
            break;
        };

        if let Ok(size) = read_half.try_read(&mut buffer) {
            if size < 1 {
                break;
            }
        }

        let clients = match SESSIONS.get(&session_token){
            Some(clients) => clients,
            None => break
        };

        for (client_id, write_half) in clients.value() {
            if *client_id == id {
                continue;
            }

            if write_half.writable().await.is_err() || write_half.try_write(&[buffer[0], *client_id]).is_err() {
                break;
            }
        }
    }

    if let Some(mut clients) = SESSIONS.get_mut(&session_token) {
        let mut i = 0;

        let vec = clients.value_mut();

        while i < vec.len() {
            let (client_id, _) = vec[i];
            if client_id == id {
                vec.remove(i);
                debug!("Client {} Removed", id);
                break;
            }
            i += 1;
        }

        if vec.len() < 1 {
            // Drop both mutable references, so that we don't cause a deadlock.
            drop(vec);
            drop(clients);
            SESSIONS.remove(&session_token);
            debug!("SESSION Closed {}", session_token);
        }
    }

    debug!("Client Disconnected");

    Ok(())
}
