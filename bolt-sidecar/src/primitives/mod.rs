// TODO: add docs
#![allow(missing_docs)]

use std::{
    borrow::Cow,
    sync::{atomic::AtomicU64, Arc},
};

use alloy::{
    primitives::{Address, U256},
    signers::k256::sha2::{Digest, Sha256},
};
use ethereum_consensus::{
    crypto::KzgCommitment,
    deneb::{
        self,
        mainnet::{BlobsBundle, MAX_BLOB_COMMITMENTS_PER_BLOCK},
        presets::mainnet::ExecutionPayloadHeader,
        Hash32,
    },
    serde::as_str,
    ssz::prelude::*,
    types::mainnet::ExecutionPayload,
    Fork,
};
use reth_primitives::{BlobTransactionSidecar, Bytes, PooledTransactionsElement, TxKind, TxType};
use serde::{de, ser::SerializeSeq, Serialize};
use tokio::sync::{mpsc, oneshot};

pub use ethereum_consensus::crypto::{PublicKey as BlsPublicKey, Signature as BlsSignature};

/// Commitment types, received by users wishing to receive preconfirmations.
pub mod commitment;
pub use commitment::{CommitmentRequest, InclusionRequest};

/// Constraint types, signed by proposers and sent along the PBS pipeline
/// for validation.
pub mod constraint;
pub use constraint::{BatchedSignedConstraints, ConstraintsMessage, SignedConstraints};
use tracing::{error, info};

use crate::crypto::SignableBLS;

/// An alias for a Beacon Chain slot number
pub type Slot = u64;

/// Minimal account state needed for commitment validation.
#[derive(Debug, Clone, Copy, Default)]
pub struct AccountState {
    /// The nonce of the account. This is the number of transactions sent from this account
    pub transaction_count: u64,
    /// The balance of the account in wei
    pub balance: U256,
    /// Flag to indicate if the account is a smart contract or an EOA
    pub has_code: bool,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct BuilderBid {
    pub header: ExecutionPayloadHeader,
    pub blob_kzg_commitments: List<KzgCommitment, MAX_BLOB_COMMITMENTS_PER_BLOCK>,
    #[serde(with = "as_str")]
    pub value: U256,
    #[serde(rename = "pubkey")]
    pub public_key: BlsPublicKey,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct SignedBuilderBid {
    pub message: BuilderBid,
    pub signature: BlsSignature,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct SignedBuilderBidWithProofs {
    pub bid: SignedBuilderBid,
    pub proofs: List<ConstraintProof, 300>,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct ConstraintProof {
    #[serde(rename = "txHash")]
    tx_hash: Hash32,
    #[serde(rename = "merkleProof")]
    merkle_proof: MerkleProof,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct MerkleProof {
    index: u64,
    // TODO: for now, max 1000
    hashes: List<Hash32, 1000>,
}

#[derive(Debug, Default, Clone, SimpleSerialize, serde::Serialize, serde::Deserialize)]
pub struct MerkleMultiProof {
    // We use List here for SSZ, TODO: choose max
    transaction_hashes: List<Hash32, 300>,
    generalized_indexes: List<u64, 300>,
    merkle_hashes: List<Hash32, 1000>,
}

#[derive(Debug)]
pub struct FetchPayloadRequest {
    pub slot: u64,
    pub response_tx: oneshot::Sender<Option<PayloadAndBid>>,
}

#[derive(Debug)]
pub struct PayloadAndBid {
    pub bid: SignedBuilderBid,
    pub payload: GetPayloadResponse,
}

#[derive(Debug, Clone)]
pub struct LocalPayloadFetcher {
    tx: mpsc::Sender<FetchPayloadRequest>,
}

impl LocalPayloadFetcher {
    pub fn new(tx: mpsc::Sender<FetchPayloadRequest>) -> Self {
        Self { tx }
    }
}

#[async_trait::async_trait]
impl PayloadFetcher for LocalPayloadFetcher {
    async fn fetch_payload(&self, slot: u64) -> Option<PayloadAndBid> {
        let (response_tx, response_rx) = oneshot::channel();

        let fetch_params = FetchPayloadRequest { response_tx, slot };
        self.tx.send(fetch_params).await.ok()?;

        match response_rx.await {
            Ok(res) => res,
            Err(e) => {
                error!(err = ?e, "Failed to fetch payload");
                None
            }
        }
    }
}

#[async_trait::async_trait]
pub trait PayloadFetcher {
    async fn fetch_payload(&self, slot: u64) -> Option<PayloadAndBid>;
}

#[derive(Debug)]
pub struct NoopPayloadFetcher;

#[async_trait::async_trait]
impl PayloadFetcher for NoopPayloadFetcher {
    async fn fetch_payload(&self, slot: u64) -> Option<PayloadAndBid> {
        info!(slot, "Fetch payload called");
        None
    }
}

/// TODO: implement SSZ
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PayloadAndBlobs {
    pub execution_payload: ExecutionPayload,
    pub blobs_bundle: BlobsBundle,
}

impl Default for PayloadAndBlobs {
    fn default() -> Self {
        Self {
            execution_payload: ExecutionPayload::Deneb(deneb::ExecutionPayload::default()),
            blobs_bundle: BlobsBundle::default(),
        }
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "version", content = "data")]
pub enum GetPayloadResponse {
    #[serde(rename = "bellatrix")]
    Bellatrix(ExecutionPayload),
    #[serde(rename = "capella")]
    Capella(ExecutionPayload),
    #[serde(rename = "deneb")]
    Deneb(PayloadAndBlobs),
    #[serde(rename = "electra")]
    Electra(PayloadAndBlobs),
}

impl GetPayloadResponse {
    pub fn block_hash(&self) -> &Hash32 {
        match self {
            GetPayloadResponse::Capella(payload) => payload.block_hash(),
            GetPayloadResponse::Bellatrix(payload) => payload.block_hash(),
            GetPayloadResponse::Deneb(payload) => payload.execution_payload.block_hash(),
            GetPayloadResponse::Electra(payload) => payload.execution_payload.block_hash(),
        }
    }

    pub fn execution_payload(&self) -> &ExecutionPayload {
        match self {
            GetPayloadResponse::Capella(payload) => payload,
            GetPayloadResponse::Bellatrix(payload) => payload,
            GetPayloadResponse::Deneb(payload) => &payload.execution_payload,
            GetPayloadResponse::Electra(payload) => &payload.execution_payload,
        }
    }
}

impl From<PayloadAndBlobs> for GetPayloadResponse {
    fn from(payload_and_blobs: PayloadAndBlobs) -> Self {
        match payload_and_blobs.execution_payload.version() {
            Fork::Phase0 => GetPayloadResponse::Capella(payload_and_blobs.execution_payload),
            Fork::Altair => GetPayloadResponse::Capella(payload_and_blobs.execution_payload),
            Fork::Capella => GetPayloadResponse::Capella(payload_and_blobs.execution_payload),
            Fork::Bellatrix => GetPayloadResponse::Bellatrix(payload_and_blobs.execution_payload),
            Fork::Deneb => GetPayloadResponse::Deneb(payload_and_blobs),
            Fork::Electra => GetPayloadResponse::Electra(payload_and_blobs),
        }
    }
}

/// A struct representing the current chain head.
#[derive(Debug, Clone)]
pub struct ChainHead {
    /// The current slot number.
    pub slot: Arc<AtomicU64>,
    /// The current block number.
    pub block: Arc<AtomicU64>,
}

impl ChainHead {
    /// Create a new ChainHead instance.
    pub fn new(slot: u64, block: u64) -> Self {
        Self { slot: Arc::new(AtomicU64::new(slot)), block: Arc::new(AtomicU64::new(block)) }
    }

    /// Get the slot number (consensus layer).
    pub fn slot(&self) -> u64 {
        self.slot.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the block number (execution layer).
    pub fn block(&self) -> u64 {
        self.block.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Trait that exposes additional information on transaction types that don't already do it
/// by themselves (e.g. [`PooledTransactionsElement`]).
pub trait TransactionExt {
    fn gas_limit(&self) -> u64;
    fn value(&self) -> U256;
    fn tx_type(&self) -> TxType;
    fn tx_kind(&self) -> TxKind;
    fn input(&self) -> &Bytes;
    fn chain_id(&self) -> Option<u64>;
    fn blob_sidecar(&self) -> Option<&BlobTransactionSidecar>;
    fn size(&self) -> usize;
}

impl TransactionExt for PooledTransactionsElement {
    fn gas_limit(&self) -> u64 {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => transaction.gas_limit,
            PooledTransactionsElement::Eip2930 { transaction, .. } => transaction.gas_limit,
            PooledTransactionsElement::Eip1559 { transaction, .. } => transaction.gas_limit,
            PooledTransactionsElement::BlobTransaction(blob_tx) => blob_tx.transaction.gas_limit,
            _ => unimplemented!(),
        }
    }

    fn value(&self) -> U256 {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => transaction.value,
            PooledTransactionsElement::Eip2930 { transaction, .. } => transaction.value,
            PooledTransactionsElement::Eip1559 { transaction, .. } => transaction.value,
            PooledTransactionsElement::BlobTransaction(blob_tx) => blob_tx.transaction.value,
            _ => unimplemented!(),
        }
    }

    fn tx_type(&self) -> TxType {
        match self {
            PooledTransactionsElement::Legacy { .. } => TxType::Legacy,
            PooledTransactionsElement::Eip2930 { .. } => TxType::Eip2930,
            PooledTransactionsElement::Eip1559 { .. } => TxType::Eip1559,
            PooledTransactionsElement::BlobTransaction(_) => TxType::Eip4844,
            _ => unimplemented!(),
        }
    }

    fn tx_kind(&self) -> TxKind {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => transaction.to,
            PooledTransactionsElement::Eip2930 { transaction, .. } => transaction.to,
            PooledTransactionsElement::Eip1559 { transaction, .. } => transaction.to,
            PooledTransactionsElement::BlobTransaction(blob_tx) => {
                TxKind::Call(blob_tx.transaction.to)
            }
            _ => unimplemented!(),
        }
    }

    fn input(&self) -> &Bytes {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => &transaction.input,
            PooledTransactionsElement::Eip2930 { transaction, .. } => &transaction.input,
            PooledTransactionsElement::Eip1559 { transaction, .. } => &transaction.input,
            PooledTransactionsElement::BlobTransaction(blob_tx) => &blob_tx.transaction.input,
            _ => unimplemented!(),
        }
    }

    fn chain_id(&self) -> Option<u64> {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => transaction.chain_id,
            PooledTransactionsElement::Eip2930 { transaction, .. } => Some(transaction.chain_id),
            PooledTransactionsElement::Eip1559 { transaction, .. } => Some(transaction.chain_id),
            PooledTransactionsElement::BlobTransaction(blob_tx) => {
                Some(blob_tx.transaction.chain_id)
            }
            _ => unimplemented!(),
        }
    }

    fn blob_sidecar(&self) -> Option<&BlobTransactionSidecar> {
        match self {
            PooledTransactionsElement::BlobTransaction(blob_tx) => Some(&blob_tx.sidecar),
            _ => None,
        }
    }

    fn size(&self) -> usize {
        match self {
            PooledTransactionsElement::Legacy { transaction, .. } => transaction.size(),
            PooledTransactionsElement::Eip2930 { transaction, .. } => transaction.size(),
            PooledTransactionsElement::Eip1559 { transaction, .. } => transaction.size(),
            PooledTransactionsElement::BlobTransaction(blob_tx) => blob_tx.transaction.size(),
            _ => unimplemented!(),
        }
    }
}

pub const fn tx_type_str(tx_type: TxType) -> &'static str {
    match tx_type {
        TxType::Legacy => "legacy",
        TxType::Eip2930 => "eip2930",
        TxType::Eip1559 => "eip1559",
        TxType::Eip4844 => "eip4844",
        TxType::Eip7702 => "eip7702",
    }
}

/// A wrapper type for a full, complete transaction (i.e. with blob sidecars attached).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FullTransaction {
    pub tx: PooledTransactionsElement,
    sender: Option<Address>,
}

impl From<PooledTransactionsElement> for FullTransaction {
    fn from(tx: PooledTransactionsElement) -> Self {
        Self { tx, sender: None }
    }
}

impl std::ops::Deref for FullTransaction {
    type Target = PooledTransactionsElement;

    fn deref(&self) -> &Self::Target {
        &self.tx
    }
}

impl std::ops::DerefMut for FullTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.tx
    }
}

impl FullTransaction {
    pub fn into_inner(self) -> PooledTransactionsElement {
        self.tx
    }

    /// Returns the sender of the transaction, if recovered.
    pub fn sender(&self) -> Option<&Address> {
        self.sender.as_ref()
    }
}

fn serialize_txs<S: serde::Serializer>(
    txs: &[FullTransaction],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let mut seq = serializer.serialize_seq(Some(txs.len()))?;
    for tx in txs {
        let encoded = tx.tx.envelope_encoded();
        seq.serialize_element(&format!("0x{}", hex::encode(encoded)))?;
    }
    seq.end()
}

fn deserialize_txs<'de, D>(deserializer: D) -> Result<Vec<FullTransaction>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let hex_strings = <Vec<Cow<'_, str>> as de::Deserialize>::deserialize(deserializer)?;
    let mut txs = Vec::with_capacity(hex_strings.len());

    for s in hex_strings {
        let data = hex::decode(s.trim_start_matches("0x")).map_err(de::Error::custom)?;
        let tx = PooledTransactionsElement::decode_enveloped(&mut data.as_slice())
            .map_err(de::Error::custom)
            .map(|tx| FullTransaction { tx, sender: None })?;
        txs.push(tx);
    }

    Ok(txs)
}

#[derive(Debug, thiserror::Error)]
#[error("Invalid signature")]
pub struct SignatureError;

#[derive(Debug, Clone, Serialize)]
pub struct SignedDelegation {
    pub message: DelegationMessage,
    pub signature: BlsSignature,
}

#[derive(Debug, Clone, Serialize)]
pub struct DelegationMessage {
    pub validator_pubkey: BlsPublicKey,
    pub delegatee_pubkey: BlsPublicKey,
}

impl SignableBLS for DelegationMessage {
    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.validator_pubkey.to_vec());
        hasher.update(&self.delegatee_pubkey.to_vec());

        hasher.finalize().into()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedRevocation {
    pub message: RevocationMessage,
    pub signature: BlsSignature,
}

#[derive(Debug, Clone, Serialize)]
pub struct RevocationMessage {
    pub validator_pubkey: BlsPublicKey,
    pub delegatee_pubkey: BlsPublicKey,
}

impl SignableBLS for RevocationMessage {
    fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(&self.validator_pubkey.to_vec());
        hasher.update(&self.delegatee_pubkey.to_vec());

        hasher.finalize().into()
    }
}
