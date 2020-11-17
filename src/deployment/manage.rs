use super::{deployment_process::DeploymentProcess, plan::Plan, recipe::DeploymentRecipe};
use crate::config::Deployment;
use crate::util::cli::ask_for_confirm;
use crate::wallet::{cli_types::LiveCell, Wallet};
use anyhow::{anyhow, Result};
use chrono::prelude::*;
use ckb_tool::ckb_types::core::{Capacity, TransactionView};
use std::fs;
use std::io::{Read, Write};
use std::path::PathBuf;

const CURRENT_SNAPSHOT: &str = "current.json";

#[derive(Clone, Debug)]
pub struct DeployOption {
    pub migrate: bool,
    pub tx_fee: Capacity,
    pub output: Option<PathBuf>,
}

/// Deployment manage
/// 1. manage migrations
/// 2, handle deploy new / rerun / migrate
pub struct Manage {
    migration_dir: PathBuf,
    deployment: Deployment,
}

impl Manage {
    pub fn new(migration_dir: PathBuf, deployment: Deployment) -> Self {
        Manage {
            migration_dir,
            deployment,
        }
    }

    /// check current snapshot
    fn check_incomplete_snapshot(&self) -> Result<()> {
        let mut path = self.migration_dir.clone();
        path.push(CURRENT_SNAPSHOT);
        if path.exists() {
            return Err(anyhow!("Find a incomplete deployment record {:?}. Please take look at the deployment record file and manualy fix the incomplete deployment.", path));
        }
        Ok(())
    }

    /// create a snapshot in migration dir
    fn snapshot_recipe(&self, recipe: &DeploymentRecipe) -> Result<PathBuf> {
        let mut path = self.migration_dir.clone();
        path.push(CURRENT_SNAPSHOT);
        let content = serde_json::to_vec(recipe)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?
            .write_all(&content)?;
        Ok(path)
    }

    /// complete a snapshot
    fn complete_snapshot(&self, src: PathBuf) -> Result<()> {
        let now: DateTime<Utc> = Utc::now();
        let snapshot_name = now.format("%Y-%m-%d-%H%M%S.json").to_string();
        let mut path = self.migration_dir.clone();
        path.push(snapshot_name);
        fs::copy(&src, &path)?;
        fs::remove_file(src)?;
        Ok(())
    }

    fn load_snapshot(&self, snapshot_name: String) -> Result<DeploymentRecipe> {
        let mut path = self.migration_dir.clone();
        path.push(snapshot_name);
        let mut buf = Vec::new();
        fs::File::open(path)?.read_to_end(&mut buf)?;
        let recipe = serde_json::from_slice(&buf)?;
        Ok(recipe)
    }

    fn collect_migration_live_cells(&self, wallet: &Wallet) -> Result<Vec<(String, LiveCell)>> {
        // read last migration
        let file_names: Vec<_> = fs::read_dir(&self.migration_dir)?
            .map(|d| d.map(|d| d.file_name()))
            .collect::<Result<_, _>>()?;
        let last_migration_file = file_names.into_iter().max();
        let mut cells = Vec::new();
        if last_migration_file.is_none() {
            return Ok(cells);
        }
        let last_migration_file = last_migration_file.unwrap();
        let recipe = self.load_snapshot(last_migration_file.into_string().unwrap())?;

        // query cells recipes
        for cell in recipe.cell_recipes {
            if let Some(tx) = wallet.query_transaction(&cell.tx_hash)? {
                let output = &tx.transaction.inner.outputs[cell.index as usize];
                let live_cell = LiveCell {
                    tx_hash: tx.transaction.hash.clone(),
                    index: cell.index,
                    capacity: output.capacity.value(),
                    mature: true,
                };
                cells.push((cell.name.clone(), live_cell));
            }
        }

        // query dep groups recipes
        for dep_group in recipe.dep_group_recipes {
            if let Some(tx) = wallet.query_transaction(&dep_group.tx_hash)? {
                let output = &tx.transaction.inner.outputs[dep_group.index as usize];
                let live_cell = LiveCell {
                    tx_hash: tx.transaction.hash.clone(),
                    index: dep_group.index,
                    capacity: output.capacity.value(),
                    mature: true,
                };
                cells.push((dep_group.name.clone(), live_cell));
            }
        }

        Ok(cells)
    }

    pub fn deploy(&self, wallet: Wallet, opt: DeployOption) -> Result<()> {
        if !self.migration_dir.exists() {
            fs::create_dir_all(&self.migration_dir)?;
            println!("Create directory {:?}", self.migration_dir);
        }
        // check incomplete snapshot
        self.check_incomplete_snapshot()?;
        let mut pre_inputs = Vec::new();
        let deployment = self.deployment.clone();
        if opt.migrate {
            pre_inputs.extend(self.collect_migration_live_cells(&wallet)?);
        }
        let mut process = DeploymentProcess::new(deployment, wallet, opt.tx_fee);
        let (recipe, txs) = process.prepare_recipe(pre_inputs.clone())?;
        if txs.is_empty() {
            return Err(anyhow!("Nothing to deploy"));
        }
        self.output_deployment_plan(&recipe, &txs, &pre_inputs, &opt);

        // only print the deployment txs
        if let Some(output_path) = opt.output.as_ref() {
            let ser_txs: Vec<ckb_tool::ckb_jsonrpc_types::Transaction> =
                txs.into_iter().map(|tx| tx.data().into()).collect();
            let content = serde_json::to_string_pretty(&ser_txs)?;
            fs::write(output_path, content)?;
            println!("Written deployment transactions to {:?}", output_path);
            return Ok(());
        }

        if ask_for_confirm("Confirm deployment?")? {
            let txs = process.sign_txs(txs)?;
            let snapshot_path = self.snapshot_recipe(&recipe)?;
            process.execute_recipe(recipe, txs)?;
            self.complete_snapshot(snapshot_path)?;
            println!("Deployment complete");
        } else {
            println!("Cancelled");
        }
        Ok(())
    }

    fn output_deployment_plan(
        &self,
        recipe: &DeploymentRecipe,
        txs: &[TransactionView],
        pre_inputs: &[(String, LiveCell)],
        opt: &DeployOption,
    ) {
        let migrated_capacity = pre_inputs
            .iter()
            .map(|(_name, cell)| cell.capacity)
            .sum::<u64>();
        let total_occupied_capacity = txs
            .iter()
            .map(|tx| {
                tx.outputs_with_data_iter()
                    .filter_map(|(output, data)| {
                        if data.is_empty() {
                            None
                        } else {
                            let data_capacity = Capacity::bytes(data.len()).expect("bytes");
                            Some(
                                output
                                    .occupied_capacity(data_capacity)
                                    .expect("occupied")
                                    .as_u64(),
                            )
                        }
                    })
                    .sum::<u64>()
            })
            .sum::<u64>();
        let new_capacity = total_occupied_capacity - migrated_capacity;
        let plan = Plan::new(
            migrated_capacity,
            new_capacity,
            total_occupied_capacity,
            opt.tx_fee.as_u64() * txs.len() as u64,
            recipe.to_owned(),
        );
        let plan = serde_yaml::to_string(&plan).unwrap();
        println!("Deployment plan:");
        println!("{}", plan);
    }
}
