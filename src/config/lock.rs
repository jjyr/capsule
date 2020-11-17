use anyhow::{anyhow, Error};
use ckb_tool::{
    ckb_jsonrpc_types::{JsonBytes, Script, ScriptHashType},
    ckb_types::{core, packed, prelude::*, H256},
};
use serde::{Deserialize, Serialize};
use std::convert::TryInto;

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MultisigLock {
    pub code_hash: H256,
    pub hash_type: ScriptHashType,
    pub format_version: u8,
    pub require_first_n_keys: u8,
    pub require_n_keys: u8,
    pub pubkey_hash_list: Vec<JsonBytes>,
}

impl MultisigLock {
    pub fn to_multisig_script(&self) -> Result<Vec<u8>, Error> {
        let s = self.format_version;
        let r = self.require_first_n_keys;
        let m = self.require_n_keys;
        if self.pubkey_hash_list.len() > std::u8::MAX as usize {
            return Err(anyhow!(
                "max length of pubkey hash list is {}, got {}",
                std::u8::MAX,
                self.pubkey_hash_list.len()
            ));
        }
        let n = self.pubkey_hash_list.len() as u8;
        let mut args = vec![s, r, m, n];

        for pubkey_hash in self.pubkey_hash_list.clone() {
            let pubkey_hash = pubkey_hash.into_bytes();
            if pubkey_hash.len() != 20 {
                return Err(anyhow!(
                    "The length of pubkey_hash is 20, got length {}",
                    pubkey_hash.len()
                ));
            }
            args.extend_from_slice(&pubkey_hash);
        }
        Ok(args)
    }
}

impl TryInto<packed::Script> for MultisigLock {
    type Error = Error;
    fn try_into(self) -> Result<packed::Script, Error> {
        let hash_type: core::ScriptHashType = self.hash_type.clone().into();

        // calculate multisig args
        let mut args = [0u8; 20];
        let multisig_script = self.to_multisig_script()?;
        let mut hasher = ckb_tool::ckb_hash::new_blake2b();
        hasher.update(&multisig_script);
        hasher.finalize(&mut args);

        Ok(packed::Script::new_builder()
            .code_hash(self.code_hash.pack())
            .hash_type(hash_type.into())
            .args(args.pack())
            .build())
    }
}

#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Lock {
    #[serde(default)]
    pub raw: Option<Script>,
    #[serde(default)]
    pub multisig: Option<MultisigLock>,
}

impl TryInto<packed::Script> for Lock {
    type Error = Error;
    fn try_into(self) -> Result<packed::Script, Error> {
        if self.raw.is_some() && self.multisig.is_some() {
            return Err(anyhow!("Can't set both raw and multsig as deployment lock"));
        }
        if let Some(raw) = self.raw {
            return Ok(raw.into());
        }
        if let Some(multisig) = self.multisig {
            return multisig.try_into();
        }
        Err(anyhow!("Can't find the deployment lock"))
    }
}
