use std::panic::UnwindSafe;

use miniscript::bitcoin::{Block, OutPoint, TxOut};

#[allow(clippy::result_unit_err)]
pub trait BlockChainClient: Send + UnwindSafe + 'static {
    fn clone(&self) -> Box<dyn BlockChainClient>;
    fn get_block(&self, height: u64) -> Result<Block, ()>;
    fn get_outpoint(&self, op: OutPoint) -> Result<TxOut, ()>;
}
