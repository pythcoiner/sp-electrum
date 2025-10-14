pub mod client;
pub mod dns;

use std::{
    io::{self, BufReader, Write},
    net::{SocketAddr, TcpStream},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use bitcoin_local::{
    consensus::{encode, Decodable},
    Network,
};
use bitcoin_p2p_messages::{
    address, message,
    message_network::{self, ClientSoftwareVersion, UserAgent, UserAgentVersion},
    Magic, ProtocolVersion, ServiceFlags,
};
use thiserror::Error;

const SOFTWARE_VERSION: ClientSoftwareVersion = ClientSoftwareVersion::SemVer {
    major: 0,
    minor: 0,
    revision: 0,
};
const USER_AGENT_VERSION: UserAgentVersion = UserAgentVersion::new(SOFTWARE_VERSION);
const SOFTWARE_NAME: &str = "sp-client";

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid peer address")]
    InvalidAddress,
    #[error("Fail to open TCP stream: {0:?}")]
    TcpConnect(io::Error),
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
}

pub fn network_to_magic(network: Network) -> Magic {
    match network {
        Network::Bitcoin => Magic::BITCOIN,
        Network::Testnet(v) => match v {
            bitcoin_local::TestnetVersion::V3 => Magic::TESTNET3,
            bitcoin_local::TestnetVersion::V4 => Magic::TESTNET4,
            _ => unreachable!("unknown testnet"),
        },
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
            message::NetworkMessage::Version(v) => {
                start_height = v.start_height;
                version = true;
            }
            message::NetworkMessage::Verack => {
                verack = true;
            }
            _ => { /* we just ignore other messages */ }
        }
        if version && verack {
            let verack_msg =
                message::RawNetworkMessage::new(magic, message::NetworkMessage::Verack);
            let _ = stream.write_all(encode::serialize(&verack_msg).as_slice());
            break;
        }
    }
    Ok((stream, reader, start_height))
}

fn version_msg(own_ip: SocketAddr, peer_ip: SocketAddr) -> message::NetworkMessage {
    let protocol_version = ProtocolVersion::from_nonstandard(60_002);

    let services = ServiceFlags::NONE;

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Time error")
        .as_secs();

    let addr_recv = address::Address::new(&peer_ip, ServiceFlags::NONE);

    let addr_from = address::Address::new(&own_ip, ServiceFlags::NONE);

    let nonce: u64 = rand::random();

    // "The last block received by the emitting node"
    let start_height: i32 = 0;

    let user_agent = UserAgent::new(SOFTWARE_NAME, USER_AGENT_VERSION);

    message::NetworkMessage::Version(message_network::VersionMessage::new(
        protocol_version,
        services,
        timestamp as i64,
        addr_recv,
        addr_from,
        nonce,
        user_agent,
        start_height,
    ))
}

#[cfg(test)]
mod tests {
    use std::{thread, time::Duration};

    use super::*;
    use bitcoin_local::BlockHash;
    use bitcoin_p2p_messages::message_blockdata::Inventory;
    use message::{InventoryPayload, NetworkMessage};

    #[test]
    fn test_p2p_handshake() {
        let _ = connect("127.0.0.1:8333", Network::Bitcoin).unwrap();
    }

    #[test]
    fn test_listen() {
        let (mut stream, mut reader, height) =
            connect("127.0.0.1:18444", Network::Regtest).unwrap();

        println!("Peer at height {height}");

        for _ in 0..10 {
            if let Ok(msg) = message::RawNetworkMessage::consensus_decode(&mut reader) {
                match msg.payload() {
                    NetworkMessage::Ping(nonce) => {
                        println!("ping");
                        let pong_msg = NetworkMessage::Pong(*nonce);
                        let msg = message::RawNetworkMessage::new(Magic::REGTEST, pong_msg);
                        let _ = stream.write_all(encode::serialize(&msg).as_slice());
                        println!("pong");

                        thread::sleep(Duration::from_millis(500));

                        let bh = BlockHash::from_str(
                            "7c754585bf936c292be6438a6aefc3ab508f373db463211800a314c657c360fc",
                        )
                        .unwrap();

                        let block = Inventory::Block(bh);
                        let getdata_msg =
                            message::NetworkMessage::GetData(InventoryPayload(vec![block]));
                        let get_block =
                            message::RawNetworkMessage::new(Magic::REGTEST, getdata_msg);
                        let _ = stream.write_all(encode::serialize(&get_block).as_slice());
                    }
                    NetworkMessage::Inv(p) => {
                        println!("{p:?}");
                        let InventoryPayload(payload) = p;
                        for e in payload {
                            if let Inventory::Block(_) = e {
                                let getdata_msg =
                                    message::NetworkMessage::GetData(InventoryPayload(vec![*e]));
                                let get_block =
                                    message::RawNetworkMessage::new(Magic::REGTEST, getdata_msg);
                                let _ = stream.write_all(encode::serialize(&get_block).as_slice());
                            }
                        }
                    }
                    NetworkMessage::Block(block) => {
                        println!("Receive block {}", block.block_hash());
                    }
                    NetworkMessage::Alert(_) => {}
                    // NetworkMessage::GetBlocks(get_blocks_message) => todo!(),
                    // NetworkMessage::Version(version_message) => todo!(),
                    // NetworkMessage::Verack => todo!(),
                    // NetworkMessage::Addr(addr_payload) => todo!(),
                    // NetworkMessage::GetData(inventory_payload) => todo!(),
                    // NetworkMessage::NotFound(inventory_payload) => todo!(),
                    NetworkMessage::GetHeaders(get_headers_message) => todo!(),
                    // NetworkMessage::MemPool => todo!(),
                    // NetworkMessage::Tx(transaction) => todo!(),
                    // NetworkMessage::Headers(headers_message) => todo!(),
                    // NetworkMessage::SendHeaders => todo!(),
                    // NetworkMessage::GetAddr => todo!(),
                    // NetworkMessage::Pong(_) => todo!(),
                    // NetworkMessage::MerkleBlock(merkle_block) => todo!(),
                    // NetworkMessage::FilterLoad(filter_load) => todo!(),
                    // NetworkMessage::FilterAdd(filter_add) => todo!(),
                    // NetworkMessage::FilterClear => todo!(),
                    // NetworkMessage::GetCFilters(get_cfilters) => todo!(),
                    // NetworkMessage::CFilter(cfilter) => todo!(),
                    // NetworkMessage::GetCFHeaders(get_cfheaders) => todo!(),
                    // NetworkMessage::CFHeaders(cfheaders) => todo!(),
                    // NetworkMessage::GetCFCheckpt(get_cfcheckpt) => todo!(),
                    // NetworkMessage::CFCheckpt(cfcheckpt) => todo!(),
                    // NetworkMessage::SendCmpct(send_cmpct) => todo!(),
                    // NetworkMessage::CmpctBlock(cmpct_block) => todo!(),
                    // NetworkMessage::GetBlockTxn(get_block_txn) => todo!(),
                    // NetworkMessage::BlockTxn(block_txn) => todo!(),
                    // NetworkMessage::Reject(reject) => todo!(),
                    // NetworkMessage::FeeFilter(fee_rate) => todo!(),
                    // NetworkMessage::WtxidRelay => todo!(),
                    // NetworkMessage::AddrV2(addr_v2_payload) => todo!(),
                    // NetworkMessage::SendAddrV2 => todo!(),
                    // NetworkMessage::Unknown { command, payload } => todo!(),
                    _ => {
                        println!("{:?}", msg.payload());
                    }
                }
            }

            // if *reply.payload() == NetworkMessage::Alert(Alert(_)) {
            //     let _ = stream.write_all(encode::serialize(&getdata_raw).as_slice());
            // } else {
            //     println!("{reply:?}");
            // }
            // println!("{reply:?}");
        }
    }
}
