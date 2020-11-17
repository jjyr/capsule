use crate::config::MultisigLock;
use anyhow::Error;
use ckb_tool::{
    ckb_hash,
    ckb_types::{
        bytes::Bytes,
        core::TransactionView,
        packed::{self, WitnessArgs},
        prelude::*,
        H256,
    },
};

pub const SIGNATURE_SIZE: usize = 65;

pub fn calculate_multisig_tx_message(
    tx: &TransactionView,
    multisig_lock: &MultisigLock,
) -> Result<H256, Error> {
    let mut blake2b = ckb_hash::new_blake2b();
    let mut message = [0u8; 32];
    let tx_hash = tx.hash();
    blake2b.update(&tx_hash.raw_data());
    let first_witness = match tx.witnesses().get(0) {
        Some(witness) => WitnessArgs::new_unchecked(witness.unpack()),
        None => WitnessArgs::default(),
    };
    let lock_without_sig = {
        let mut buf = multisig_lock.to_multisig_script()?;
        let sig_len = multisig_lock.pubkey_hash_list.len() * SIGNATURE_SIZE;
        buf.resize(buf.len() + sig_len, 0);
        buf
    };
    let first_witness_without_sig = first_witness
        .clone()
        .as_builder()
        .lock(Some(Bytes::from(lock_without_sig)).pack())
        .build();
    let len = first_witness_without_sig.as_bytes().len() as u64;
    // hash first witness
    blake2b.update(&len.to_le_bytes());
    blake2b.update(&first_witness_without_sig.as_bytes());

    // hash rest witnesses
    (1..std::cmp::max(tx.witnesses().len(), tx.inputs().len())).for_each(|n| {
        let witness: Bytes = tx.witnesses().get(n).unwrap_or_default().unpack();
        let len = witness.len() as u64;
        blake2b.update(&len.to_le_bytes());
        blake2b.update(&witness);
    });
    blake2b.finalize(&mut message);
    Ok(message.into())
}

pub fn put_signatures_on_multisig_tx(
    tx: &TransactionView,
    multisig_lock: &MultisigLock,
    signatures: &[[u8; SIGNATURE_SIZE]],
) -> Result<TransactionView, Error> {
    let signed_witnesses: Vec<packed::Bytes> = tx
        .inputs()
        .into_iter()
        .enumerate()
        .map(|(i, _)| {
            if i == 0 {
                let mut unlock_args = multisig_lock.to_multisig_script()?;
                signatures.iter().for_each(|sig| {
                    unlock_args.extend_from_slice(sig);
                });
                let witness =
                    WitnessArgs::new_unchecked(tx.witnesses().get(i).unwrap_or_default().unpack());
                Ok(witness
                    .as_builder()
                    .lock(Some(Bytes::from(unlock_args)).pack())
                    .build()
                    .as_bytes()
                    .pack())
            } else {
                Ok(tx.witnesses().get(i).unwrap_or_default())
            }
        })
        .collect::<Result<_, Error>>()?;
    Ok(tx
        .as_advanced_builder()
        .set_witnesses(signed_witnesses)
        .build())
}
