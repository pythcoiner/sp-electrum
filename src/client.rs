use std::{collections::BTreeMap, panic::UnwindSafe, thread, time::Duration};

use bwk_electrum::{
    electrum::{request::Request, response::TxGetResponse},
    raw_client::Client,
};
use miniscript::bitcoin::{Block, OutPoint, Transaction, TxOut, Txid};

#[allow(clippy::result_unit_err)]
pub trait BlockChainClient: Send + UnwindSafe + 'static {
    fn clone(&self) -> Box<dyn BlockChainClient>;
    fn get_block(&mut self, height: u64) -> Result<Block, ()>;
    fn get_outpoint(&mut self, op: OutPoint, block_height: u64) -> Result<TxOut, ()>;
}

const SSL_PREFIX: &str = "ssl://";
const TX_CACHE_SIZE: usize = 1000;

#[derive(Debug, Clone)]
pub struct ElectrumClient {
    url: String,
    port: u16,
    inner: Option<Client>,
    tx_cache: BTreeMap<Txid, Transaction>,
    request_id: usize,
}

impl BlockChainClient for ElectrumClient {
    fn clone(&self) -> Box<dyn BlockChainClient> {
        Box::new(Self {
            url: self.url.clone(),
            port: self.port,
            inner: None,
            tx_cache: BTreeMap::new(),
            request_id: 0,
        })
    }

    fn get_block(&mut self, height: u64) -> Result<Block, ()> {
        self.clear_cache_maybe();
        self._get_block(height)
    }

    fn get_outpoint(&mut self, op: OutPoint, _block_height: u64) -> Result<TxOut, ()> {
        let index = op.vout as usize;
        // Try to get from the cache first
        if let Some(tx) = self.tx_cache.get(&op.txid) {
            return tx.output.get(index).ok_or(()).cloned();
        }
        // else fetch it
        let tx = self.get_tx(&op.txid)?;
        let vout = tx.output.get(index).ok_or(()).cloned();

        // and populate the cache
        self.tx_cache.insert(op.txid, tx);

        vout
    }
}

impl ElectrumClient {
    #[allow(clippy::result_unit_err)]
    pub fn connect_maybe(&mut self) -> Result<(), ()> {
        for t in 1..10 {
            let conn = if let Some(inner) = &mut self.inner {
                if !inner.is_connected() {
                    inner.try_connect().map_err(|_| ())
                } else {
                    return Ok(());
                }
            } else {
                self.connect()
            };
            if conn.is_ok() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis((t * t) * 100));
        }
        Err(())
    }

    #[allow(clippy::result_unit_err)]
    pub fn connect(&mut self) -> Result<(), ()> {
        if self.inner.is_some() {
            return Ok(());
        }
        let ssl = self.url.starts_with(SSL_PREFIX);
        let url = if ssl {
            self.url.replacen(SSL_PREFIX, "", 1)
        } else {
            self.url.clone()
        };
        let mut inner = Client::new_ssl_maybe(&url, self.port, ssl);
        if inner.try_connect().is_ok() {
            self.inner = Some(inner);
            return Ok(());
        }
        Err(())
    }

    pub fn clear_cache_maybe(&mut self) {
        if self.tx_cache.len() > TX_CACHE_SIZE {
            self.tx_cache.clear();
        }
    }

    #[allow(clippy::result_unit_err)]
    pub fn _get_block(&mut self, _height: u64) -> Result<Block, ()> {
        // TODO: find a way to fetch blocks from bitcoin P2P network:
        //  - get/verify headers from electrum
        //  - download block from P2P network
        //  - verify block
        //
        todo!()
    }

    #[allow(clippy::result_unit_err)]
    pub fn get_tx(&mut self, txid: &Txid) -> Result<Transaction, ()> {
        self.connect_maybe()?;
        let req = Request::tx_get(*txid).id(self.request_id);
        self.request_id += 1;
        for t in 1..10 {
            if let Some(inner) = self.inner.as_mut() {
                // send request
                if inner.try_send(&req).is_ok() {
                    // wait for response
                    if let Ok(raw) = inner.recv_str() {
                        let tx: Result<TxGetResponse, _> = serde_json::from_str(&raw);
                        if let Ok(resp) = tx {
                            match resp.result {
                                bwk_electrum::electrum::response::TxGetResult::Raw(raw_tx) => {
                                    let tx: Transaction =
                                        serde_json::from_str(&raw_tx).map_err(|_| ())?;
                                    return Ok(tx);
                                }
                                _ => return Err(()),
                            }
                        }
                    }
                }
            }

            thread::sleep(Duration::from_millis((t * t) * 10));
        }
        Err(())
    }
}
