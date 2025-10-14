use crate::{
    threading::{Context, ThreadPool},
    transaction,
};
use miniscript::bitcoin::{Amount, Block, OutPoint, Transaction, TxOut};
use silentpayments::{
    receiving::Label,
    secp256k1::{Scalar, XOnlyPublicKey},
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::mpsc::{self},
};

pub enum Error {
    Block,
    TxOut,
}

pub fn block_select_eligible_transactions(
    height: u64,
    block: Block,
    dust: Amount,
) -> (
    u64, /* block height */
    Vec<(Transaction, Vec<bool> /* input eligible */)>,
) {
    let mut out = vec![];
    for tx in block.txdata {
        let (eligible, inputs) = transaction::tx_is_sp_eligible(&tx, dust);
        if eligible {
            out.push((tx, inputs));
        }
    }
    (height, out)
}

#[derive(Clone)]
pub struct SpendableTx {
    pub height: u64,
    pub tx: Transaction,
    pub map: HashMap<Option<Label>, HashMap<XOnlyPublicKey, Scalar>>,
}

pub enum Notification {
    StartScan(u64 /* start height */, u64 /* stop height */),
    StartBlock(u64 /* block height */),
    Transaction(SpendableTx),
    EndBlock(u64 /* block height */),
    Aborted(u64 /* aborted height */),
    Error((u64 /* block height */, Error)),
}

pub enum TxOutRequest {
    OutPoint,
    TxOut(OutPoint, TxOut),
}

pub fn block_scan(
    pool: &ThreadPool,
    height: u64,
    block: Block,
    notif: mpsc::Sender<Notification>,
    context: &Context,
) {
    notif
        .send(Notification::StartBlock(height))
        .expect("closed");

    let (_, txs) = block_select_eligible_transactions(height, block, context.dust);

    // Fetch all funding outputs in parallel
    let (sender, receiver) = mpsc::channel();
    for (tx, inputs) in &txs {
        for (i, fetch) in inputs.iter().enumerate() {
            if *fetch {
                let op = tx.input[i].previous_output;
                let sender = sender.clone();
                let notif = notif.clone();
                pool.execute(move |c| {
                    sender.send(TxOutRequest::OutPoint).expect("poisoned");
                    let vout = match c.client.get_outpoint(op, height) {
                        Ok(vout) => vout,
                        Err(_) => {
                            notif
                                .send(Notification::Error((height, Error::TxOut)))
                                .expect("closed");
                            return c;
                        }
                    };
                    sender.send(TxOutRequest::TxOut(op, vout)).expect("closed");
                    c
                });
            }
        }
    }

    // Collect all funding outputs
    let mut vouts = BTreeMap::new();
    let mut count = 0usize;
    loop {
        match receiver.recv().expect("closed") {
            TxOutRequest::OutPoint => count += 1,
            TxOutRequest::TxOut(op, tx_out) => {
                vouts.insert(op, tx_out);
                let count = count.saturating_sub(1);
                if count == 0 {
                    break;
                }
            }
        }
    }

    // Regroups txs w/ their required funding outputs and process txs in parallel
    let (sender, receiver) = mpsc::channel();
    for (tx, inputs) in txs {
        let mut spent_outputs = BTreeMap::new();
        for (i, fetch) in inputs.iter().enumerate() {
            if *fetch {
                let op = tx.input[i].previous_output;
                // FIXME: can we take it instead of clone?
                let vout = vouts.get(&op).expect("must be present").clone();
                spent_outputs.insert(i, vout);
            }
        }
        let sender = sender.clone();
        sender.send(1).expect("closed");
        let notif = notif.clone();
        pool.execute(move |c| {
            transaction::tx_scan(height, tx, spent_outputs, &notif, &c);
            sender.send(-1).expect("closed");
            c
        });
    }

    // wait for all threads to finnish
    let mut threads = 0;
    loop {
        threads += receiver.recv().expect("closed");
        if threads == 0 {
            break;
        }
    }

    notif.send(Notification::EndBlock(height)).expect("closed");
}
