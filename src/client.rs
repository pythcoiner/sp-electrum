use std::{collections::BTreeMap, net::SocketAddr, panic::UnwindSafe, thread, time::Duration};

use crate::p2p;
use bwk_electrum::electrum::{
    request::Request,
    response::{HeaderResponse, TxGetResponse},
};
use miniscript::bitcoin::{
    block::Header as BlockHeader, Block, Network, OutPoint, Transaction, TxOut, Txid,
};

#[allow(clippy::result_unit_err)]
pub trait BlockChainClient: Send + UnwindSafe + 'static {
    fn clone(&self) -> Box<dyn BlockChainClient>;
    fn get_block(&mut self, height: u64) -> Result<Block, ()>;
    fn get_outpoint(&mut self, op: OutPoint, block_height: u64) -> Result<TxOut, ()>;
}

const SSL_PREFIX: &str = "ssl://";
const TX_CACHE_SIZE: usize = 1000;
const TRY: u64 = 10;

pub struct Error;

#[derive(Debug, Clone)]
pub struct ElectrumClient {
    electrum_url: String,
    electrum_port: u16,
    p2p_addr: SocketAddr,
    inner_electrum: Option<bwk_electrum::raw_client::Client>,
    inner_p2p: Option<p2p::Client>,
    tx_cache: BTreeMap<Txid, Transaction>,
    request_id: usize,
    network: Network,
}

impl BlockChainClient for ElectrumClient {
    fn clone(&self) -> Box<dyn BlockChainClient> {
        Box::new(Self {
            electrum_url: self.electrum_url.clone(),
            electrum_port: self.electrum_port,
            p2p_addr: self.p2p_addr,
            inner_electrum: None,
            inner_p2p: None,
            tx_cache: BTreeMap::new(),
            request_id: 0,
            network: self.network,
        })
    }

    fn get_block(&mut self, height: u64) -> Result<Block, ()> {
        self.clear_cache_maybe();
        self._get_block(height).map_err(|_| ())
    }

    fn get_outpoint(&mut self, op: OutPoint, _block_height: u64) -> Result<TxOut, ()> {
        let index = op.vout as usize;
        // Try to get from the cache first
        if let Some(tx) = self.tx_cache.get(&op.txid) {
            return tx.output.get(index).ok_or(()).cloned();
        }
        // else fetch it
        let tx = self.get_tx(&op.txid).map_err(|_| ())?;
        let vout = tx.output.get(index).ok_or(()).cloned();

        // and populate the cache
        self.tx_cache.insert(op.txid, tx);

        vout
    }
}

impl ElectrumClient {
    pub fn connect_maybe(&mut self) -> Result<(), Error> {
        self.connect_electrum_maybe()?;
        self.connect_p2p_maybe()
    }
    fn connect_electrum_maybe(&mut self) -> Result<(), Error> {
        for t in 1..TRY {
            let conn = if let Some(inner) = self.inner_electrum.as_mut() {
                if !inner.is_connected() {
                    inner.try_connect().map_err(|_| Error)
                } else {
                    return Ok(());
                }
            } else {
                self.connect_electrum()
            };
            if conn.is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis((t * t) * 100));
        }
        Err(Error)
    }

    fn connect_p2p_maybe(&mut self) -> Result<(), Error> {
        for t in 1..TRY {
            let conn = if let Some(inner) = self.inner_p2p.as_mut() {
                if !inner.is_started() {
                    let new = inner.clone().connect().map_err(|_| Error)?;
                    *inner = new;
                    Ok(())
                } else {
                    return Ok(());
                }
            } else {
                self.connect_electrum()
            };
            if conn.is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis((t * t) * 100));
        }
        Err(Error)
    }

    fn connect_p2p(&mut self) -> Result<(), Error> {
        if self.inner_p2p.is_some() {
            return Ok(());
        }
        let client = p2p::Client::new(self.p2p_addr, self.network)
            .connect()
            .map_err(|_| Error)?;
        self.inner_p2p = Some(client);
        Ok(())
    }

    fn connect_electrum(&mut self) -> Result<(), Error> {
        if self.inner_electrum.is_some() {
            return Ok(());
        }
        let ssl = self.electrum_url.starts_with(SSL_PREFIX);
        let url = if ssl {
            self.electrum_url.replacen(SSL_PREFIX, "", 1)
        } else {
            self.electrum_url.clone()
        };
        let mut inner =
            bwk_electrum::raw_client::Client::new_ssl_maybe(&url, self.electrum_port, ssl);
        if inner.try_connect().is_ok() {
            self.inner_electrum = Some(inner);
            return Ok(());
        }
        Err(Error)
    }

    fn clear_cache_maybe(&mut self) {
        if self.tx_cache.len() > TX_CACHE_SIZE {
            self.tx_cache.clear();
        }
    }

    fn _get_block(&mut self, height: u64) -> Result<Block, Error> {
        self.connect_maybe()?;
        // NOTE: the only way to get the BlockHash w/ electrum api is to get the next header,
        // thus we cannot get the last block hash
        let req = Request::header((height + 1) as usize).id(self.request_id);
        self.request_id += 1;
        for t in 1..TRY {
            if let (Some(electrum), Some(p2p)) =
                (self.inner_electrum.as_mut(), self.inner_p2p.as_mut())
            {
                let mut block_hash = None;
                if electrum.try_send(&req).is_ok() {
                    if let Ok(raw) = electrum.recv_str() {
                        let tx: Result<HeaderResponse, _> = serde_json::from_str(&raw);
                        if let Ok(HeaderResponse { raw_header, .. }) = tx {
                            let header: BlockHeader = match serde_json::from_str(&raw_header) {
                                Ok(h) => h,
                                Err(_) => return Err(Error),
                            };
                            block_hash = Some(header.block_hash());
                        } else {
                            return Err(Error);
                        }
                    }
                }
                if let Some(block_hash) = block_hash {
                    if let Ok(Some(block)) = p2p.get_block(block_hash) {
                        return Ok(block);
                    } else {
                        return Err(Error);
                    }
                }
            }
            thread::sleep(Duration::from_millis((t * t) * 10));
        }
        Err(Error)
    }

    fn get_tx(&mut self, txid: &Txid) -> Result<Transaction, Error> {
        self.connect_maybe()?;
        let req = Request::tx_get(*txid).id(self.request_id);
        self.request_id += 1;
        for t in 1..TRY {
            if let Some(inner) = self.inner_electrum.as_mut() {
                // send request
                if inner.try_send(&req).is_ok() {
                    // wait for response
                    if let Ok(raw) = inner.recv_str() {
                        let tx: Result<TxGetResponse, _> = serde_json::from_str(&raw);
                        if let Ok(resp) = tx {
                            match resp.result {
                                bwk_electrum::electrum::response::TxGetResult::Raw(raw_tx) => {
                                    let tx: Transaction =
                                        serde_json::from_str(&raw_tx).map_err(|_| Error)?;
                                    return Ok(tx);
                                }
                                _ => return Err(Error),
                            }
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis((t * t) * 10));
        }
        Err(Error)
    }
}
