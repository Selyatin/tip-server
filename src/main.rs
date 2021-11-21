#[macro_use] extern crate log;
#[macro_use] extern crate lazy_static;

use std::{
    sync::{Arc, atomic::{AtomicUsize, Ordering}},
    time::{Instant, Duration}
};
use tokio::{
    net::{TcpListener, TcpStream, tcp::{OwnedReadHalf, OwnedWriteHalf}},
    io::{self, AsyncWriteExt, Sink},
    sync::broadcast::{self, Receiver, Sender, error::RecvError}
};
use rand::Rng;
use dashmap::DashMap;

lazy_static! {
    static ref SESSIONS: DashMap<u16, Sender<Arc<[u8]>>> = DashMap::default();
}

static CLIENTS_COUNT: AtomicUsize = AtomicUsize::new(0);

#[tokio::main]
async fn main() -> io::Result<()>{
    env_logger::init();

    let addr = "0.0.0.0:8080";

    let listener = TcpListener::bind(addr).await?;

    info!("Listener Initialised at {}", addr);

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
) -> io::Result<()> {
    let mut id: u8 = 0;
    let mut session_token: Option<u16> = None;

    debug!("Client Connected");

    loop {
        read_half.readable().await?;

        debug!("Readable");

        let mut buffer = [0u8; 6];

        if let Ok(size) = read_half.try_read(&mut buffer) {
            if size < 1 {
                debug!("Packet empty");
                break;
            }
        }

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
                let (sender, _) = broadcast::channel(4);
                SESSIONS.insert(token, sender);
                session_token = Some(token);
                tokio::spawn(async move { 
                    if let Err(err) = forward_notification_loop(session_token.unwrap()).await { 
                        error!("{}", err); 
                    }
                });
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

            if let Some(sender) = SESSIONS.get(&session_token.unwrap()){
                id = sender.receiver_count() as u8;

                if id > 3 {
                    return Ok(());
                }

                write_half.writable().await?;
                write_half.try_write(&[id])?;

                let mut message = [b'J', b'o', b'i', b'n', id];

                for i in 0..id {
                    write_half.writable().await?;
                    message[4] = i;
                    write_half.try_write(&message)?;
                }

                let message = Arc::new(message);

                if sender.send(message).is_err() {
                    drop(sender);
                    SESSIONS.remove(&session_token.unwrap());
                    return Ok(());
                }
            }
            break;
        }
    }

    let session_token = match session_token {
        Some(token) => token,
        None => return Ok(())
    };

    info!("Clients Connected: {}", CLIENTS_COUNT.fetch_add(1, Ordering::Relaxed) + 1);

    debug!("Session Token {}", session_token);

    let mut receiver = SESSIONS.get(&session_token).unwrap().subscribe();

    tokio::spawn(async move {
        if let Err(err) = write_half_loop(write_half, receiver, id).await {
            error!("Write Half: {}", err);
        }
    });
    
    tokio::spawn(async move {
        if let Err(err) = read_half_loop(read_half, session_token, id).await {
            error!("Read Half: {}", err);
        }
        if let Some(sender) = SESSIONS.get(&session_token) {
            sender.send(Arc::new([b'L', b'e', b'f', b't', id])).ok();
        }
    });

    Ok(())
}

async fn read_half_loop(read_half: OwnedReadHalf, session_token: u16, id: u8) -> io::Result<()> {

    while read_half.readable().await.is_ok() {
        let mut buffer = [0u8; 1];

        let size = match read_half.try_read(&mut buffer){
            Ok(size) => size,
            Err(err) => {
                if err.kind() == io::ErrorKind::WouldBlock {
                    continue;
                }
                return Err(err);
            }
        };

        if size < 1 {
            break;
        }

        let sender = SESSIONS.get(&session_token).unwrap();
        
        if sender.send(Arc::new([id, buffer[0]])).is_err() {
            break;
        }
    }

    Ok(())
}

async fn write_half_loop(write_half: OwnedWriteHalf, mut receiver: Receiver<Arc<[u8]>>, id: u8) -> io::Result<()> {
    loop {
        let message = match receiver.recv().await {
            Ok(message) => message,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => break
        };
        
        if message[0] == id {
            continue;
        }

        write_half.writable().await?;
        write_half.try_write(&message)?;
    }

    CLIENTS_COUNT.fetch_sub(1, Ordering::Relaxed);

    Ok(())
}

/// Notifies the players that the game must proceed forward, i.e the words will go forward in the x
/// axis
async fn forward_notification_loop(session_token: u16) -> io::Result<()> {
    let message = Arc::new([b'+']);

    tokio::time::sleep(Duration::from_secs(5)).await;

    while let Some(sender) = SESSIONS.get(&session_token) {
        debug!("Notifying Clients to move Forward");

        if sender.send(message.clone()).is_err() {
            drop(sender);
            SESSIONS.remove(&session_token);
            break;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    
    info!("Session {} Cleared", session_token);

    Ok(())
}
