use cml_chain::byron::ByronTxOut;
use cml_core::serialization::{FromBytes, Serialize};
use cml_crypto::RawBytesEncoding;
use entity::block::EraValue;
use sea_orm::DbErr;
use std::collections::BTreeMap;

use super::common::{
    build_asset, filter_outputs_and_datums_by_address, filter_outputs_and_datums_by_hash,
    reduce_ada_amount, Dex, DexType, MinSwapV1, QueuedMeanPrice, QueuedSwap,
};
use crate::multiera::dex::common::datum_to_json;
use crate::multiera::utils::common::output_from_bytes;
use crate::{era_common::OutputWithTxData, multiera::utils::common::get_asset_amount};
use entity::dex_swap::Operation;

pub const POOL_SCRIPT_HASH1: &str = "e1317b152faac13426e6a83e06ff88a4d62cce3c1634ab0a5ec13309";
pub const POOL_SCRIPT_HASH2: &str = "57c8e718c201fba10a9da1748d675b54281d3b1b983c5d1687fc7317";
pub const BATCH_ORDER_ADDRESS1: &str = "addr1wyx22z2s4kasd3w976pnjf9xdty88epjqfvgkmfnscpd0rg3z8y6v";
pub const BATCH_ORDER_ADDRESS2: &str = "addr1wxn9efv2f6w82hagxqtn62ju4m293tqvw0uhmdl64ch8uwc0h43gt";
pub const SWAP_IN_ADA: u64 = 4_000_000; // oil ADA + agent fee
pub const SWAP_OUT_ADA: u64 = 2_000_000; // oil ADA

impl Dex for MinSwapV1 {
    fn queue_mean_price(
        &self,
        queued_prices: &mut Vec<QueuedMeanPrice>,
        tx: &cml_multi_era::MultiEraTransactionBody,
        tx_witness: &cml_chain::transaction::TransactionWitnessSet,
        tx_id: i64,
    ) -> Result<(), String> {
        // Note: there should be at most one pool output
        if let Some((output, datum)) = filter_outputs_and_datums_by_hash(
            &tx.outputs(),
            &[POOL_SCRIPT_HASH1, POOL_SCRIPT_HASH2],
            &tx_witness.plutus_datums,
        )
        .first()
        {
            let datum = datum_to_json(datum)?;

            let parse_asset_item = |i, j| -> Result<Vec<u8>, &str> {
                let item = datum["fields"][i]["fields"][j]["bytes"]
                    .as_str()
                    .ok_or("Failed to parse asset item")?
                    .to_string();
                hex::decode(item).map_err(|_e| "Failed to parse asset item")
            };
            let asset1 = build_asset(parse_asset_item(0, 0)?, parse_asset_item(0, 1)?);
            let asset2 = build_asset(parse_asset_item(1, 0)?, parse_asset_item(1, 1)?);

            let amount1 = get_asset_amount(output, &asset1);
            let amount2 = get_asset_amount(output, &asset2);

            queued_prices.push(QueuedMeanPrice {
                tx_id,
                address: output.address().to_raw_bytes().to_vec(),
                dex_type: DexType::MinSwapV1,
                asset1,
                asset2,
                amount1,
                amount2,
            });
        }
        Ok(())
    }

    fn queue_swap(
        &self,
        queued_swaps: &mut Vec<QueuedSwap>,
        tx: &cml_multi_era::MultiEraTransactionBody,
        tx_witness: &cml_chain::transaction::TransactionWitnessSet,
        tx_id: i64,
        multiera_used_inputs_to_outputs_map: &BTreeMap<Vec<u8>, BTreeMap<i64, OutputWithTxData>>,
    ) -> Result<(), String> {
        // Note: there should be at most one pool output
        if let Some((main_output, main_datum)) = filter_outputs_and_datums_by_hash(
            &tx.outputs(),
            &[POOL_SCRIPT_HASH1, POOL_SCRIPT_HASH2],
            &tx_witness.plutus_datums,
        )
        .first()
        {
            let main_datum = datum_to_json(main_datum)?;

            let mut free_utxos: Vec<cml_multi_era::utils::MultiEraTransactionOutput> = tx.outputs();

            // Extract asset information from plutus data of pool input
            let parse_asset_item = |i, j| -> Result<Vec<u8>, &str> {
                let item = main_datum["fields"][i]["fields"][j]["bytes"]
                    .as_str()
                    .ok_or("Failed to parse asset item")?
                    .to_string();
                hex::decode(item).map_err(|_e| "Failed to parse asset item")
            };
            let asset1 = build_asset(parse_asset_item(0, 0)?, parse_asset_item(0, 1)?);
            let asset2 = build_asset(parse_asset_item(1, 0)?, parse_asset_item(1, 1)?);

            let inputs: Vec<cml_multi_era::utils::MultiEraTransactionOutput> = tx
                .inputs()
                .iter()
                .map(|i| {
                    let output = &multiera_used_inputs_to_outputs_map
                        [&i.hash().unwrap().to_raw_bytes().to_vec()]
                        [&(i.index().unwrap() as i64)];
                    output_from_bytes(output).unwrap()
                })
                .collect::<Vec<_>>();
            for (input, input_datum) in filter_outputs_and_datums_by_address(
                &inputs,
                &[BATCH_ORDER_ADDRESS1, BATCH_ORDER_ADDRESS2],
                &tx_witness.plutus_datums,
            ) {
                let input_datum = datum_to_json(&input_datum)?;

                // identify operation: 0 = swap
                let operation = input_datum["fields"][3]["constructor"]
                    .as_i64()
                    .ok_or("Failed to parse operation")?;
                if operation != 0 {
                    tracing::debug!("Operation is not a swap");
                    continue;
                }

                let parse_asset_item = |i, j| -> Result<Vec<u8>, &str> {
                    let item = input_datum["fields"][3]["fields"][i]["fields"][j]["bytes"]
                        .as_str()
                        .ok_or("Failed to parse asset item")?
                        .to_string();
                    hex::decode(item).map_err(|_e| "Failed to parse asset item")
                };
                let target_asset = build_asset(parse_asset_item(0, 0)?, parse_asset_item(0, 1)?);

                // Get transaction output
                let output_address_items = [
                    String::from("01"), // mainnet
                    input_datum["fields"][1]["fields"][0]["fields"][0]["bytes"]
                        .as_str()
                        .ok_or("Failed to parse output address item")?
                        .to_string(),
                    input_datum["fields"][1]["fields"][1]["fields"][0]["fields"][0]["fields"][0]
                        ["bytes"]
                        .as_str()
                        .ok_or("Failed to parse output address item")?
                        .to_string(),
                ];
                let output_address =
                    cml_chain::address::Address::from_hex(&output_address_items.join(""))
                        .map_err(|_e| "Failed to parse output address")?;

                // Get coresponding UTxO with result
                let utxo_pos = free_utxos
                    .iter()
                    .position(|o| o.address() == output_address.clone())
                    .ok_or("Failed to find utxo")?;
                let utxo = free_utxos[utxo_pos].clone();
                free_utxos.remove(utxo_pos);

                // Get amount and direction
                let amount1;
                let amount2;
                let operation;
                if target_asset == asset2 {
                    amount1 =
                        get_asset_amount(&input, &asset1) - reduce_ada_amount(&asset1, SWAP_IN_ADA);
                    amount2 =
                        get_asset_amount(&utxo, &asset2) - reduce_ada_amount(&asset2, SWAP_OUT_ADA);
                    operation = Operation::Sell;
                } else {
                    amount1 =
                        get_asset_amount(&utxo, &asset1) - reduce_ada_amount(&asset1, SWAP_OUT_ADA);
                    amount2 =
                        get_asset_amount(&input, &asset2) - reduce_ada_amount(&asset2, SWAP_IN_ADA);
                    operation = Operation::Buy
                }
                queued_swaps.push(QueuedSwap {
                    tx_id,
                    address: main_output.address().to_raw_bytes().to_vec(),
                    dex_type: DexType::MinSwapV1,
                    asset1: asset1.clone(),
                    asset2: asset2.clone(),
                    amount1,
                    amount2,
                    operation,
                })
            }
        }
        Ok(())
    }
}
