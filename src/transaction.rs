use std::{collections::BTreeMap, sync::mpsc};

use miniscript::bitcoin::{Amount, Transaction, TxIn, TxOut};
use silentpayments::{secp256k1::XOnlyPublicKey, utils::NUMS_H};

use crate::{
    block::{Notification, SpendableTx},
    threading::Context,
};

pub fn tx_is_sp_eligible(transaction: &Transaction, dust: Amount) -> (bool, Vec<bool>) {
    let mut _continue = false;
    for out in &transaction.output {
        if output_is_sp_eligible(out) && out.value >= dust {
            _continue = true;
            break;
        }
    }
    if !_continue {
        return (false, vec![]);
    }

    let mut input_eligible = vec![];
    for inp in &transaction.input {
        match input_pubkey(inp) {
            InputType::Eligible => input_eligible.push(true),
            InputType::NotEligible => input_eligible.push(false),
            InputType::Forbidden => return (false, vec![]),
        }
    }
    (input_eligible.iter().any(|i| *i), input_eligible)
}

pub enum InputType {
    Eligible,
    NotEligible,
    Forbidden,
}

#[inline]
pub fn output_is_sp_eligible(output: &TxOut) -> bool {
    output.script_pubkey.is_p2tr()
}

#[inline]
pub fn input_pubkey(input: &TxIn) -> InputType {
    if input_is_sp_forbidden(input) {
        return InputType::Forbidden;
    }
    if input_is_sp_elligible(input) {
        return InputType::Eligible;
    }
    InputType::NotEligible
}

#[inline]
pub fn input_is_sp_forbidden(_input: &TxIn) -> bool {
    // TODO: if segwit version > 1 => false
    // NOTE: for now skip this step as we need to fetch the spk
    false
}

#[inline]
pub fn input_is_sp_elligible(input: &TxIn) -> bool {
    input.script_sig.is_p2pkh()
        || input.script_sig.is_p2wpkh()
        || input_is_p2sh_p2wpkh(input)
        || input_is_taproot_sp_elligible(input)
}

#[inline]
pub fn input_is_taproot_sp_elligible(input: &TxIn) -> bool {
    if !input.script_sig.is_p2tr() {
        return false;
    }
    let witness = &input.witness;
    match (witness.is_empty(), input.script_sig.is_empty()) {
        (false, true) => {
            // check for the optional annex
            let annex = match witness.last().and_then(|value| value.first()) {
                Some(&0x50) => 1,
                Some(_) => 0,
                None => return false, /* unreachable */
            };

            // Check for script path
            let stack_size = witness.len();
            if stack_size > annex && witness[stack_size - annex - 1][1..33] == NUMS_H {
                return false;
            }
            true
        }
        _ => false,
    }
}

#[inline]
pub fn input_is_p2sh_p2wpkh(input: &TxIn) -> bool {
    if !input.script_sig.is_p2sh() {
        return false;
    }
    // P2SH-P2WPKH redeem script is always:
    // OP_0 (0x00) + push 0x14 (20) + 20-byte pubkey hash  → 23 bytes total
    let scr = input.script_sig.as_bytes();
    scr.len() == 23 && scr[0] == 0x00 && scr[1] == 0x14
}

pub fn tx_scan(
    height: u64,
    tx: Transaction,
    // NOTE: only TxOut matching an eligible input must be provided
    spent_outputs: BTreeMap<usize, TxOut>,
    notif: &mpsc::Sender<Notification>,
    context: &Context,
    // b_scan: SecretKey,
    // receiver: Arc<Receiver>,
    // secp: Arc<Secp256k1<secp256k1::All>>,
) {
    let mut input_pubkeys = vec![];
    let mut outpoints = vec![];
    for (i, vout) in spent_outputs {
        let input = match tx.input.get(i) {
            Some(inp) => inp,
            // TODO: handle error
            None => todo!("invalid spent_outputs"),
        };
        let spk = vout.script_pubkey;
        let pubkey = match silentpayments::utils::receiving::get_pubkey_from_input(
            input.script_sig.as_bytes(),
            &input.witness.to_vec(),
            spk.as_bytes(),
        ) {
            Ok(Some(pk)) => pk,
            // TODO: handle error
            _ => todo!("invalid spent_outputs"),
        };
        input_pubkeys.push(pubkey);
        // FIXME: making calculate_tweak_data() take an OutPoint must save some cpu cost here
        let outpoint = (
            input.previous_output.txid.to_string(),
            input.previous_output.vout,
        );
        outpoints.push(outpoint);
    }

    let pubkeys_ref: Vec<&_> = input_pubkeys.iter().collect();

    let tweak_data =
        match silentpayments::utils::receiving::calculate_tweak_data(&pubkeys_ref, &outpoints) {
            Ok(tw) => tw,
            // TODO: handle error
            Err(_) => todo!("tweak fail"),
        };

    let ecdh_shared_secret = silentpayments::utils::receiving::calculate_ecdh_shared_secret(
        &tweak_data,
        &context.b_scan,
    );

    let pubkeys_to_check: Vec<_> = tx
        .output
        .iter()
        .filter(|o| o.script_pubkey.is_p2tr())
        .map(|o| {
            XOnlyPublicKey::from_slice(&o.script_pubkey.as_bytes()[2..])
                .expect("P2tr output should have a valid xonly key")
        })
        .collect();

    let map = match context.sp_receiver.scan_transaction(
        &ecdh_shared_secret,
        pubkeys_to_check,
        &context.secp,
    ) {
        Ok(r) => r,
        // TODO: handle error
        Err(_) => todo!("scan failed"),
    };

    if !map.is_empty() {
        let tx = SpendableTx { height, tx, map };
        notif.send(Notification::Transaction(tx)).expect("closed");
    }
}
