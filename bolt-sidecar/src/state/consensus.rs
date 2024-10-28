use std::{
    fmt,
    time::{Duration, Instant},
};

use beacon_api_client::{mainnet::Client, ProposerDuty};
use ethereum_consensus::{crypto::PublicKey as BlsPublicKey, phase0::mainnet::SLOTS_PER_EPOCH};
use tokio::join;
use tracing::debug;

use super::CommitmentDeadline;
use crate::{
    config::ValidatorIndexes,
    primitives::{CommitmentRequest, Slot},
    telemetry::ApiMetrics,
    BeaconClient,
};

/// Consensus-related errors
#[derive(Debug, thiserror::Error)]
#[allow(missing_docs)]
#[non_exhaustive]
pub enum ConsensusError {
    #[error("Beacon API error: {0}")]
    BeaconApiError(#[from] beacon_api_client::Error),
    #[error("Invalid slot: {0}")]
    InvalidSlot(Slot),
    #[error("Inclusion deadline exceeded")]
    DeadlineExceeded,
    #[error("Validator not found in the slot")]
    ValidatorNotFound,
}

/// Represents an epoch in the beacon chain.
#[derive(Debug, Default)]
struct Epoch {
    /// The epoch number
    pub value: u64,
    /// The start slot of the epoch
    pub start_slot: Slot,
    /// The proposer duties of the epoch.
    ///
    /// NOTE: if the unsafe lookhead flag is enabled, then this field represents the proposer
    /// duties also for the next epoch.
    pub proposer_duties: Vec<ProposerDuty>,
}

/// Represents the consensus state container for the sidecar.
#[allow(missing_debug_implementations)]
pub struct ConsensusState {
    beacon_api_client: Client,
    epoch: Epoch,
    validator_indexes: ValidatorIndexes,
    // Timestamp of when the latest slot was received
    latest_slot_timestamp: Instant,
    // The latest slot received
    latest_slot: Slot,
    /// The deadline (expressed in seconds) in the slot for which to
    /// stop accepting commitments.
    ///
    /// This is used to prevent the sidecar from accepting commitments
    /// which won't have time to be included by the PBS pipeline.
    // commitment_deadline: u64,
    pub commitment_deadline: CommitmentDeadline,
    /// The duration of the commitment deadline.
    commitment_deadline_duration: Duration,
    /// If commitment requests should be validated against also against the unsafe lookahead
    pub unsafe_lookahead_enabled: bool,
}

impl fmt::Debug for ConsensusState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConsensusState")
            .field("epoch", &self.epoch)
            .field("latest_slot", &self.latest_slot)
            .field("latest_slot_timestamp", &self.latest_slot_timestamp)
            .field("commitment_deadline", &self.commitment_deadline)
            .finish()
    }
}

impl ConsensusState {
    /// Create a new `ConsensusState` with the given configuration.
    pub fn new(
        beacon_api_client: BeaconClient,
        validator_indexes: ValidatorIndexes,
        commitment_deadline_duration: Duration,
        unsafe_lookahead_enabled: bool,
    ) -> Self {
        ConsensusState {
            beacon_api_client,
            validator_indexes,
            epoch: Epoch::default(),
            latest_slot: Default::default(),
            latest_slot_timestamp: Instant::now(),
            commitment_deadline: CommitmentDeadline::new(0, commitment_deadline_duration),
            commitment_deadline_duration,
            unsafe_lookahead_enabled,
        }
    }

    /// This function validates the state of the chain against a block. It checks 2 things:
    /// 1. The target slot is one of our proposer slots. (TODO)
    /// 2. The request hasn't passed the slot deadline.
    ///
    /// If the request is valid, it returns the validator public key for the slot.
    /// TODO: Integrate with the registry to check if we are registered.
    pub fn validate_request(
        &self,
        request: &CommitmentRequest,
    ) -> Result<BlsPublicKey, ConsensusError> {
        let CommitmentRequest::Inclusion(req) = request;

        let furthest_slot = self.epoch.start_slot
            + SLOTS_PER_EPOCH
            + if self.unsafe_lookahead_enabled { SLOTS_PER_EPOCH } else { 0 };

        // Check if the slot is in the current epoch or next epoch (if unsafe lookahead is enabled)
        if req.slot < self.epoch.start_slot || req.slot >= furthest_slot {
            return Err(ConsensusError::InvalidSlot(req.slot));
        }

        // If the request is for the next slot, check if it's within the commitment deadline
        if req.slot == self.latest_slot + 1
            && self.latest_slot_timestamp + self.commitment_deadline_duration < Instant::now()
        {
            return Err(ConsensusError::DeadlineExceeded);
        }

        // Find the validator index for the given slot
        let validator_pubkey = self.find_validator_pubkey_for_slot(req.slot)?;

        Ok(validator_pubkey)
    }

    /// Update the latest head and fetch the relevant data from the beacon chain.
    pub async fn update_slot(&mut self, slot: u64) -> Result<(), ConsensusError> {
        debug!("Updating slot to {slot}");
        ApiMetrics::set_latest_head(slot as u32);

        // Reset the commitment deadline to start counting for the next slot.
        self.commitment_deadline =
            CommitmentDeadline::new(slot + 1, self.commitment_deadline_duration);

        // Update the timestamp with current time
        self.latest_slot_timestamp = Instant::now();
        self.latest_slot = slot;

        // Calculate the current value of epoch
        let epoch = slot / SLOTS_PER_EPOCH;

        // If the epoch has changed, update the proposer duties
        if epoch != self.epoch.value {
            debug!("Updating epoch to {epoch}");
            self.epoch.value = epoch;
            self.epoch.start_slot = epoch * SLOTS_PER_EPOCH;

            self.fetch_proposer_duties(epoch).await?;
        } else if self.epoch.proposer_duties.is_empty() {
            debug!(epoch, "No proposer duties found for current epoch, fetching...");
            // If the proposer duties are empty, fetch them
            self.fetch_proposer_duties(epoch).await?;
        }

        Ok(())
    }

    /// Fetch proposer duties for the given epoch and the next one if the unsafe lookahead flag is set
    async fn fetch_proposer_duties(&mut self, epoch: u64) -> Result<(), ConsensusError> {
        let duties = if self.unsafe_lookahead_enabled {
            let two_epoch_duties = join!(
                self.beacon_api_client.get_proposer_duties(epoch),
                self.beacon_api_client.get_proposer_duties(epoch + 1)
            );

            match two_epoch_duties {
                (Ok((_, mut duties)), Ok((_, next_duties))) => {
                    duties.extend(next_duties);
                    duties
                }
                (Err(e), _) | (_, Err(e)) => return Err(ConsensusError::BeaconApiError(e)),
            }
        } else {
            self.beacon_api_client.get_proposer_duties(epoch).await?.1
        };

        self.epoch.proposer_duties = duties;

        Ok(())
    }

    /// Finds the validator public key for the given slot from the proposer duties.
    fn find_validator_pubkey_for_slot(&self, slot: u64) -> Result<BlsPublicKey, ConsensusError> {
        self.epoch
            .proposer_duties
            .iter()
            .find(|&duty| {
                duty.slot == slot && self.validator_indexes.contains(duty.validator_index as u64)
            })
            .map(|duty| duty.public_key.clone())
            .ok_or(ConsensusError::ValidatorNotFound)
    }
}

#[cfg(test)]
mod tests {
    use beacon_api_client::{BlockId, ProposerDuty};
    use reqwest::Url;
    use tracing::warn;

    use super::*;
    use crate::test_util::try_get_beacon_api_url;

    #[tokio::test]
    async fn test_find_validator_index_for_slot() {
        // Sample proposer duties
        let proposer_duties = vec![
            ProposerDuty { public_key: Default::default(), slot: 1, validator_index: 100 },
            ProposerDuty { public_key: Default::default(), slot: 2, validator_index: 101 },
            ProposerDuty { public_key: Default::default(), slot: 3, validator_index: 102 },
        ];

        // Validator indexes that we are interested in
        let validator_indexes = ValidatorIndexes::from(vec![100, 102]);

        // Create a ConsensusState with the sample proposer duties and validator indexes
        let state = ConsensusState {
            beacon_api_client: Client::new(Url::parse("http://localhost").unwrap()),
            epoch: Epoch { value: 0, start_slot: 0, proposer_duties },
            latest_slot_timestamp: Instant::now(),
            commitment_deadline: CommitmentDeadline::new(0, Duration::from_secs(1)),
            validator_indexes,
            commitment_deadline_duration: Duration::from_secs(1),
            latest_slot: 0,
            unsafe_lookahead_enabled: false,
        };

        // Test finding a valid slot
        assert_eq!(state.find_validator_pubkey_for_slot(1).unwrap(), Default::default());
        assert_eq!(state.find_validator_pubkey_for_slot(3).unwrap(), Default::default());

        // Test finding an invalid slot (not in proposer duties)
        assert!(matches!(
            state.find_validator_pubkey_for_slot(4),
            Err(ConsensusError::ValidatorNotFound)
        ));
    }

    #[tokio::test]
    async fn test_update_slot() -> eyre::Result<()> {
        let _ = tracing_subscriber::fmt::try_init();

        let commitment_deadline_duration = Duration::from_secs(1);
        let validator_indexes = ValidatorIndexes::from(vec![100, 101, 102]);

        let Some(url) = try_get_beacon_api_url().await else {
            warn!("skipping test: beacon API URL is not reachable");
            return Ok(());
        };

        let beacon_client = BeaconClient::new(Url::parse(url).unwrap());

        // Create the initial ConsensusState
        let mut state = ConsensusState {
            beacon_api_client: beacon_client,
            epoch: Epoch::default(),
            latest_slot: Default::default(),
            latest_slot_timestamp: Instant::now(),
            validator_indexes,
            commitment_deadline: CommitmentDeadline::new(0, commitment_deadline_duration),
            commitment_deadline_duration,
            unsafe_lookahead_enabled: false,
        };

        // Update the slot to 32
        state.update_slot(32).await.unwrap();

        // Check values were updated correctly
        assert_eq!(state.latest_slot, 32);
        assert!(state.latest_slot_timestamp.elapsed().as_secs() < 1);
        assert_eq!(state.epoch.value, 1);
        assert_eq!(state.epoch.start_slot, 32);

        // Update the slot to 63, which should not update the epoch
        state.update_slot(63).await.unwrap();

        // Check values were updated correctly
        assert_eq!(state.latest_slot, 63);
        assert!(state.latest_slot_timestamp.elapsed().as_secs() < 1);
        assert_eq!(state.epoch.value, 1);
        assert_eq!(state.epoch.start_slot, 32);

        Ok(())
    }

    #[tokio::test]
    async fn test_fetch_proposer_duties() -> eyre::Result<()> {
        let _ = tracing_subscriber::fmt::try_init();

        let Some(url) = try_get_beacon_api_url().await else {
            warn!("skipping test: beacon API URL is not reachable");
            return Ok(());
        };

        let beacon_client = BeaconClient::new(Url::parse(url).unwrap());

        let commitment_deadline_duration = Duration::from_secs(1);

        // Create the initial ConsensusState
        let mut state = ConsensusState {
            beacon_api_client: beacon_client,
            epoch: Epoch::default(),
            latest_slot: Default::default(),
            latest_slot_timestamp: Instant::now(),
            validator_indexes: Default::default(),
            commitment_deadline: CommitmentDeadline::new(0, commitment_deadline_duration),
            commitment_deadline_duration,
            // We test for both epochs
            unsafe_lookahead_enabled: true,
        };

        let epoch =
            state.beacon_api_client.get_beacon_header(BlockId::Head).await?.header.message.slot
                / SLOTS_PER_EPOCH;

        state.fetch_proposer_duties(epoch).await?;
        assert_eq!(state.epoch.proposer_duties.len(), SLOTS_PER_EPOCH as usize * 2);

        Ok(())
    }
}
