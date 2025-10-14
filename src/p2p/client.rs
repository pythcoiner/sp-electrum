use std::{
    io::{BufReader, Write},
    net::{SocketAddr, TcpStream},
    sync::{mpsc, Arc, Mutex},
    time::{Duration, SystemTime},
};

use super::{network_to_magic, Error};
use bitcoin_local::{absolute::Decodable, consensus::encode, Block, BlockHash, Network};
use bitcoin_p2p_messages::{
    message::{self, InventoryPayload, NetworkMessage},
    message_blockdata::Inventory,
    Magic,
};

pub struct Client {
    address: SocketAddr,
    network: Network,
    timeout: Duration,
    stream: Option<Arc<Mutex<TcpStream>>>,
    receiver: Option<mpsc::Receiver<NetworkMessage>>,
}

impl Client {
    pub fn new(address: SocketAddr, network: Network) -> Self {
        Self {
            address,
            network,
            timeout: Duration::from_millis(1000),
            stream: None,
            receiver: None,
        }
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn connect(&mut self) -> Result<(), Error> {
        let (stream, reader, _height) =
            crate::p2p::connect(&self.address.to_string(), self.network)?;
        let stream = Arc::new(Mutex::new(stream));
        self.stream = Some(stream.clone());
        self.start_listen(reader, stream)
    }

    fn start_listen(
        &mut self,
        reader: BufReader<TcpStream>,
        stream: Arc<Mutex<TcpStream>>,
    ) -> Result<(), Error> {
        if !self.is_connected() {
            return Err(Error::Connected);
        }

        let (thread_sender, client_receiver) = mpsc::channel();
        self.receiver = Some(client_receiver);

        let magic = network_to_magic(self.network);

        std::thread::spawn(move || {
            Self::listen(stream, reader, thread_sender, magic);
        });

        Ok(())
    }

    fn listen(
        stream: Arc<Mutex<TcpStream>>,
        mut reader: BufReader<TcpStream>,
        sender: mpsc::Sender<NetworkMessage>,
        magic: Magic,
    ) {
        if let Ok(msg) = message::RawNetworkMessage::consensus_decode(&mut reader) {
            match msg.into_payload() {
                NetworkMessage::Ping(nonce) => {
                    let pong_msg = NetworkMessage::Pong(nonce);
                    let msg = message::RawNetworkMessage::new(magic, pong_msg);
                    stream
                        .lock()
                        .expect("poisoned")
                        .write_all(encode::serialize(&msg).as_slice())
                        .unwrap();
                }
                m @ NetworkMessage::Block(_) => {
                    sender.send(m).unwrap();
                }
                _ => {}
            }
        }
    }

    pub fn stop(&mut self) {
        self.stream = None;
    }

    pub fn is_started(&self) -> bool {
        self.receiver.is_some()
    }

    pub fn is_connected(&self) -> bool {
        self.stream.is_some() || self.is_started()
    }

    pub fn get_block(&mut self, block_hash: BlockHash) -> Result<Option<Block>, Error> {
        if !self.is_connected() {
            return Err(Error::Connected);
        }
        let stream = if let Some(stream) = &self.stream {
            stream.clone()
        } else {
            return Err(Error::Connected);
        };
        let timeout = self.timeout;
        let mut back_off = bwk_backoff::Backoff::new_ms(20);
        if let Some(receiver) = self.receiver.as_mut() {
            let block = Inventory::Block(block_hash);
            let getdata_msg = message::NetworkMessage::GetData(InventoryPayload(vec![block]));
            let magic = network_to_magic(self.network);
            let get_block = message::RawNetworkMessage::new(magic, getdata_msg);
            let _ = stream
                .lock()
                .expect("poisoned")
                .write_all(encode::serialize(&get_block).as_slice());
            let send = SystemTime::now();

            loop {
                if let Ok(NetworkMessage::Block(block)) = receiver.try_recv() {
                    return Ok(Some(block));
                } else {
                    let elapsed = SystemTime::now().duration_since(send).expect("valid time");
                    if elapsed > timeout {
                        return Ok(None);
                    } else {
                        back_off.snooze();
                    }
                }
            }
        } else {
            Err(Error::Connected)
        }
    }
}
