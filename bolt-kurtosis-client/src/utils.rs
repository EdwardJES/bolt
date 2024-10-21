use std::str::FromStr;

use alloy::{
    consensus::{BlobTransactionSidecar, SidecarBuilder, SimpleCoder},
    network::TransactionBuilder,
    primitives::{Address, U256},
    rpc::types::TransactionRequest,
};
use beacon_api_client::{mainnet::Client as BeaconApiClient, BlockId};
use eyre::Result;
use rand::{thread_rng, Rng};
use serde_json::Value;

use crate::constants::{DEAD_ADDRESS, KURTOSIS_CHAIN_ID, NOICE_GAS_PRICE};

/// Generates random ETH transfer to `DEAD_ADDRESS` with a random payload.
pub fn generate_random_tx() -> TransactionRequest {
    TransactionRequest::default()
        .with_to(Address::from_str(DEAD_ADDRESS).unwrap())
        .with_chain_id(KURTOSIS_CHAIN_ID)
        .with_value(U256::from(thread_rng().gen_range(1..100)))
        .with_gas_limit(21_000u128)
        .with_gas_price(NOICE_GAS_PRICE)
}

/// Generate random transaction with blob (eip4844)
pub fn generate_random_blob_tx() -> TransactionRequest {
    let random_bytes = thread_rng().gen::<[u8; 32]>();
    let sidecar: SidecarBuilder<SimpleCoder> = SidecarBuilder::from_slice(random_bytes.as_slice());
    let sidecar: BlobTransactionSidecar = sidecar.build().unwrap();

    let dead_address = Address::from_str(DEAD_ADDRESS).unwrap();

    TransactionRequest::default()
        .with_to(dead_address)
        .with_chain_id(KURTOSIS_CHAIN_ID)
        .with_value(U256::from(100))
        .with_max_fee_per_blob_gas(100u128)
        .max_fee_per_gas(NOICE_GAS_PRICE)
        .max_priority_fee_per_gas(NOICE_GAS_PRICE)
        .with_gas_limit(42_000u128)
        .with_blob_sidecar(sidecar)
        .with_input(random_bytes)
}

pub fn prepare_rpc_request(method: &str, params: Vec<Value>) -> Value {
    serde_json::json!({
        "id": "1",
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Returns the current slot
pub async fn current_slot(beacon_api_client: &BeaconApiClient) -> Result<u64> {
    let current_slot =
        beacon_api_client.get_beacon_header(BlockId::Head).await?.header.message.slot;
    Ok(current_slot)
}
