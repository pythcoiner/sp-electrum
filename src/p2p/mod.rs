pub mod client;
pub mod dns;

#[allow(unused)]
pub use client::Client;

use std::{
    io::{BufReader, Write},
    net::{SocketAddr, TcpStream},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use bitcoin::p2p::{
    address,
    message::{self, NetworkMessage},
    message_network::VersionMessage,
    Magic, ServiceFlags,
};
use bitcoin::{
    consensus::{encode, Decodable},
    Network,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid peer address")]
    InvalidAddress,
    #[error("Fail to open TCP stream: {0:?}")]
    TcpConnect(std::io::Error),
    #[error("Peer response is wrong")]
    WrongPeerResponse,
    #[error("Fail to decode peer response")]
    Decode,
    #[error("Fail to clone the stream")]
    Stream,
    #[error("Fail to get local ip address")]
    LocalAddress,
    #[error("The P2P client is not connected")]
    Connected,
    #[error("The client stopped unexpectedly")]
    Stopped,
}

pub fn network_to_magic(network: Network) -> Magic {
    match network {
        Network::Bitcoin => Magic::BITCOIN,
        Network::Testnet => Magic::TESTNET3,
        Network::Testnet4 => Magic::TESTNET4,
        Network::Signet => Magic::SIGNET,
        Network::Regtest => Magic::REGTEST,
    }
}

// TODO: add timeout
fn connect(
    addr: &str,
    network: Network,
) -> Result<(TcpStream, BufReader<TcpStream>, i32 /* height */), Error> {
    let address = SocketAddr::from_str(addr).map_err(|_| Error::InvalidAddress)?;
    let magic = network_to_magic(network);

    let mut stream = TcpStream::connect(address).map_err(Error::TcpConnect)?;
    let own_ip = stream.local_addr().map_err(|_| Error::LocalAddress)?;

    let version_message = version_msg(own_ip, address);
    let version_message = message::RawNetworkMessage::new(magic, version_message);

    let _ = stream.write_all(encode::serialize(&version_message).as_slice());

    let mut reader = BufReader::new(stream.try_clone().map_err(|_| Error::Stream)?);

    let mut verack = false;
    let mut version = false;
    let mut start_height = 0;
    loop {
        let reply =
            message::RawNetworkMessage::consensus_decode(&mut reader).map_err(|_| Error::Decode)?;
        match reply.payload() {
            NetworkMessage::Version(v) => {
                start_height = v.start_height;
                version = true;
            }
            NetworkMessage::Verack => {
                verack = true;
            }
            _ => { /* we just ignore other messages */ }
        }
        if version && verack {
            let verack_msg = message::RawNetworkMessage::new(magic, NetworkMessage::Verack);
            let _ = stream.write_all(encode::serialize(&verack_msg).as_slice());
            break;
        }
    }
    Ok((stream, reader, start_height))
}

fn version_msg(own_ip: SocketAddr, peer_ip: SocketAddr) -> NetworkMessage {
    let services = ServiceFlags::NONE;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time error")
        .as_secs();

    let addr_recv = address::Address::new(&peer_ip, ServiceFlags::NONE);

    let addr_from = address::Address::new(&own_ip, ServiceFlags::NONE);

    let nonce: u64 = rand::random();

    // The last block received by the emitting node
    let start_height: i32 = 0;

    let client_name = "sp-electrum";
    let client_version = "0.0.0";
    let user_agent = format!("/{client_name}:{client_version}/");

    let mut version = VersionMessage::new(
        services,
        timestamp as i64,
        addr_recv,
        addr_from,
        nonce,
        user_agent,
        start_height,
    );
    version.version = 60_002;

    NetworkMessage::Version(version)
}
