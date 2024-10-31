use std::{
    collections::HashSet,
    fmt,
    time::{Duration, Instant},
};

use alloy::{rpc::types::beacon::events::HeadEvent, signers::local::PrivateKeySigner};
use beacon_api_client::mainnet::Client as BeaconClient;
use ethereum_consensus::{
    clock::{self, SlotStream, SystemTimeProvider},
    crypto::bls::PublicKey as BlsPublicKey,
    phase0::mainnet::SLOTS_PER_EPOCH,
};
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::{
    builder::payload_fetcher::LocalPayloadFetcher,
    chain_io::manager::BoltManager,
    commitments::{
        server::{CommitmentsApiServer, Event as CommitmentEvent},
        spec::Error as CommitmentError,
    },
    crypto::{bls::cl_public_key_to_arr, SignableBLS, SignerECDSA},
    primitives::{
        read_signed_delegations_from_file, CommitmentRequest, ConstraintsMessage,
        FetchPayloadRequest, SignedConstraints, TransactionExt,
    },
    signer::{keystore::KeystoreSigner, local::LocalSigner},
    start_builder_proxy_server,
    state::{fetcher::StateFetcher, ConsensusState, ExecutionState, HeadTracker, StateClient},
    telemetry::ApiMetrics,
    BuilderProxyConfig, CommitBoostSigner, ConstraintsApi, ConstraintsClient, LocalBuilder, Opts,
    SignerBLS,
};

/// The driver for the sidecar, responsible for managing the main event loop.
///
/// The reponsibilities of the driver include:
/// - Handling incoming API events
/// - Updating the execution state based on new beacon chain heads
/// - Submitting constraints to the constraints service at the commitment deadline
/// - Building local payloads for the beacon chain
/// - Responding to requests to fetch a local payload
/// - Updating the consensus state based on the beacon chain clock
pub struct SidecarDriver<C, ECDSA> {
    /// Head tracker for monitoring the beacon chain clock
    head_tracker: HeadTracker,
    /// Execution state for tracking the current head and block templates
    execution: ExecutionState<C>,
    /// Consensus state for tracking the current slot and validator indexes
    consensus: ConsensusState,
    /// Signer for creating constraints
    constraint_signer: SignerBLS,
    /// Signer for creating commitment responses
    commitment_signer: ECDSA,
    /// Local block builder for creating local payloads
    local_builder: LocalBuilder,
    /// Client for interacting with the constraints service
    constraints_client: ConstraintsClient,
    /// Channel for receiving incoming API events
    api_events_rx: mpsc::Receiver<CommitmentEvent>,
    /// Channel for receiving requests to fetch a local payload
    payload_requests_rx: mpsc::Receiver<FetchPayloadRequest>,
    /// Stream of slots made from the consensus clock
    slot_stream: SlotStream<SystemTimeProvider>,
}

impl fmt::Debug for SidecarDriver<StateClient, PrivateKeySigner> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SidecarDriver")
            .field("head_tracker", &self.head_tracker)
            .field("execution", &self.execution)
            .field("consensus", &self.consensus)
            .field("constraint_signer", &self.constraint_signer)
            .field("commitment_signer", &self.commitment_signer)
            .field("local_builder", &self.local_builder)
            .field("constraints_client", &self.constraints_client)
            .field("api_events_rx", &self.api_events_rx)
            .field("payload_requests_rx", &self.payload_requests_rx)
            .finish()
    }
}

impl SidecarDriver<StateClient, PrivateKeySigner> {
    /// Create a new sidecar driver with the given [Opts] and private key signer.
    pub async fn with_local_signer(opts: &Opts) -> eyre::Result<Self> {
        // The default state client simply uses the execution API URL to fetch state updates.
        let state_client = StateClient::new(opts.execution_api_url.clone());

        // Constraints are signed with a BLS private key
        let constraint_signer = SignerBLS::Local(LocalSigner::new(
            opts.constraint_signing
                .constraint_private_key
                .clone()
                .expect("local constraint signing key")
                .0,
            opts.chain,
        ));

        // Commitment responses are signed with a regular Ethereum wallet private key.
        let commitment_key = opts.commitment_private_key.0.clone();
        let commitment_signer = PrivateKeySigner::from_signing_key(commitment_key);

        Self::from_components(opts, constraint_signer, commitment_signer, state_client).await
    }
}

impl SidecarDriver<StateClient, PrivateKeySigner> {
    /// Create a new sidecar driver with the given [Opts] and keystore signer.
    pub async fn with_keystore_signer(opts: &Opts) -> eyre::Result<Self> {
        // The default state client simply uses the execution API URL to fetch state updates.
        let state_client = StateClient::new(opts.execution_api_url.clone());

        let keystore = if let Some(psw) = opts.constraint_signing.keystore_password.as_ref() {
            KeystoreSigner::from_password(
                opts.constraint_signing.keystore_path.as_ref().expect("keystore path"),
                psw.as_ref(),
                opts.chain,
            )?
        } else {
            KeystoreSigner::from_secrets_directory(
                opts.constraint_signing.keystore_path.as_ref().expect("keystore path"),
                opts.constraint_signing.keystore_secrets_path.as_ref().expect("keystore secrets"),
                opts.chain,
            )?
        };

        let keystore_signer = SignerBLS::Keystore(keystore);

        // Commitment responses are signed with a regular Ethereum wallet private key.
        let commitment_key = opts.commitment_private_key.0.clone();
        let commitment_signer = PrivateKeySigner::from_signing_key(commitment_key);

        Self::from_components(opts, keystore_signer, commitment_signer, state_client).await
    }
}

impl SidecarDriver<StateClient, CommitBoostSigner> {
    /// Create a new sidecar driver with the given [Opts] and commit-boost signer.
    pub async fn with_commit_boost_signer(opts: &Opts) -> eyre::Result<Self> {
        // The default state client simply uses the execution API URL to fetch state updates.
        let state_client = StateClient::new(opts.execution_api_url.clone());

        let commit_boost_signer = CommitBoostSigner::new(
            opts.constraint_signing.commit_boost_signer_url.clone().expect("CommitBoost URL"),
            &opts.constraint_signing.commit_boost_jwt_hex.clone().expect("CommitBoost JWT"),
        )?;

        let cb_bls_signer = SignerBLS::CommitBoost(commit_boost_signer.clone());

        Self::from_components(opts, cb_bls_signer, commit_boost_signer, state_client).await
    }
}

impl<C: StateFetcher, ECDSA: SignerECDSA> SidecarDriver<C, ECDSA> {
    /// Create a new sidecar driver with the given components
    pub async fn from_components(
        opts: &Opts,
        constraint_signer: SignerBLS,
        commitment_signer: ECDSA,
        fetcher: C,
    ) -> eyre::Result<Self> {
        let mut constraints_client = ConstraintsClient::new(opts.constraints_api_url.clone());

        // read the delegations from disk if they exist and add them to the constraints client.
        let validator_public_keys = if let Some(delegations_file_path) =
            opts.constraint_signing.delegations_path.as_ref()
        {
            let delegations = read_signed_delegations_from_file(delegations_file_path)?;
            let validator_public_keys =
                delegations.iter().map(|d| d.message.validator_pubkey.clone()).collect::<Vec<_>>();
            constraints_client.add_delegations(delegations);
            validator_public_keys
        } else {
            Vec::from_iter(constraint_signer.available_pubkeys())
        };

        // Verify the operator and validator keys with the bolt manager
        if let Some(bolt_manager) =
            BoltManager::from_chain(opts.execution_api_url.clone(), opts.chain.chain)
        {
            let commitment_signer_pubkey = commitment_signer.public_key();
            let validator_public_keys_len = validator_public_keys.len();
            info!(
                validator_public_keys_len,
                commitment_signer_pubkey = ?commitment_signer_pubkey,
                "Verifying validators and operator keys with Bolt Manager, this may take a while..."
            );
            bolt_manager
                .verify_validator_pubkeys(validator_public_keys, commitment_signer_pubkey)
                .await?;
            info!(
                validator_public_keys_len,
                commitment_signer_pubkey = ?commitment_signer_pubkey,
                "Verified validators and operator keys verified with Bolt Manager successfully"
            );
        } else {
            warn!(
                "No Bolt Manager contract deployed on {} chain, skipping validators and operator public keys verification",
                opts.chain.name()
            );
        }

        let beacon_client = BeaconClient::new(opts.beacon_api_url.clone());
        let execution = ExecutionState::new(fetcher, opts.limits).await?;

        let genesis_time = beacon_client.get_genesis_details().await?.genesis_time;
        let slot_stream =
            clock::from_system_time(genesis_time, opts.chain.slot_time(), SLOTS_PER_EPOCH)
                .into_stream();

        let local_builder = LocalBuilder::new(opts, beacon_client.clone(), genesis_time);
        let head_tracker = HeadTracker::start(beacon_client.clone());

        let consensus = ConsensusState::new(
            beacon_client,
            opts.validator_indexes.clone(),
            opts.chain.commitment_deadline(),
            opts.chain.enable_unsafe_lookahead,
        );

        let (payload_requests_tx, payload_requests_rx) = mpsc::channel(16);
        let builder_proxy_cfg = BuilderProxyConfig {
            constraints_client: constraints_client.clone(),
            server_port: opts.constraints_proxy_port,
        };

        // start the builder api proxy server
        tokio::spawn(async move {
            let payload_fetcher = LocalPayloadFetcher::new(payload_requests_tx);
            if let Err(err) = start_builder_proxy_server(payload_fetcher, builder_proxy_cfg).await {
                error!(?err, "Builder API proxy server failed");
            }
        });

        // start the commitments api server
        let api_addr = format!("0.0.0.0:{}", opts.port);
        let (api_events_tx, api_events_rx) = mpsc::channel(1024);
        CommitmentsApiServer::new(api_addr).run(api_events_tx).await;

        Ok(SidecarDriver {
            head_tracker,
            execution,
            consensus,
            constraint_signer,
            commitment_signer,
            local_builder,
            constraints_client,
            api_events_rx,
            payload_requests_rx,
            slot_stream,
        })
    }

    /// Run the main event loop endlessly for the sidecar driver.
    ///
    /// Any errors encountered are contained to the specific `handler` in which
    /// they occurred, and the driver will continue to run as long as possible.
    pub async fn run_forever(mut self) -> ! {
        const BOLT: &str = r#"
            ██████╗  ██████╗ ██╗  ████████╗
            ██╔══██╗██╔═══██╗██║  ╚══██╔══╝
            ██████╔╝██║   ██║██║     ██║   
            ██╔══██╗██║   ██║██║     ██║   
            ██████╔╝╚██████╔╝███████╗██║   
            ╚═════╝  ╚═════╝ ╚══════╝╚═╝   "#;
        println!("{BOLT}");

        loop {
            tokio::select! {
                Some(api_event) = self.api_events_rx.recv() => {
                    self.handle_incoming_api_event(api_event).await;
                }
                Ok(head_event) = self.head_tracker.next_head() => {
                    self.handle_new_head_event(head_event).await;
                }
                Some(slot) = self.consensus.commitment_deadline.wait() => {
                    self.handle_commitment_deadline(slot).await;
                }
                Some(payload_request) = self.payload_requests_rx.recv() => {
                    self.handle_fetch_payload_request(payload_request);
                }
                Some(slot) = self.slot_stream.next() => {
                    if let Err(e) = self.consensus.update_slot(slot).await {
                        error!(err = ?e, "Failed to update consensus state slot");
                    }
                }
            }
        }
    }

    /// Handle an incoming API event, validating the request and responding with a commitment.
    async fn handle_incoming_api_event(&mut self, event: CommitmentEvent) {
        let CommitmentEvent { mut request, response } = event;
        info!("Received new commitment request: {:?}", request);
        ApiMetrics::increment_inclusion_commitments_received();

        let start = Instant::now();

        let validator_pubkey = match self.consensus.validate_request(&request) {
            Ok(pubkey) => pubkey,
            Err(err) => {
                error!(?err, "Consensus: failed to validate request");
                let _ = response.send(Err(CommitmentError::Consensus(err)));
                return;
            }
        };

        if let Err(err) = self.execution.validate_request(&mut request).await {
            error!(?err, "Execution: failed to commit request");
            ApiMetrics::increment_validation_errors(err.to_tag_str().to_owned());
            let _ = response.send(Err(CommitmentError::Validation(err)));
            return;
        }

        // TODO: match when we have more request types
        let CommitmentRequest::Inclusion(inclusion_request) = request.clone();
        let target_slot = inclusion_request.slot;

        info!(
            target_slot,
            elapsed = ?start.elapsed(),
            "Validation against execution state passed"
        );

        let delegatees = self.constraints_client.find_delegatees(&validator_pubkey);
        let available_pubkeys = self.constraint_signer.available_pubkeys();

        let Some(pubkey) = pick_public_key(validator_pubkey, available_pubkeys, delegatees) else {
            error!(%target_slot, "No available public key to sign constraints with");
            let _ = response.send(Err(CommitmentError::Internal));
            return;
        };

        // NOTE: we iterate over the transactions in the request and generate a signed constraint
        // for each one. This is because the transactions in the commitment request are not
        // supposed to be treated as a relative-ordering bundle, but a batch
        // with no ordering guarantees.
        for tx in inclusion_request.txs {
            let tx_type = tx.tx_type();
            let message = ConstraintsMessage::from_transaction(pubkey.clone(), target_slot, tx);
            let digest = message.digest();

            let signature = match self.constraint_signer {
                SignerBLS::Local(ref signer) => signer.sign_commit_boost_root(digest),
                SignerBLS::CommitBoost(ref signer) => signer.sign_commit_boost_root(digest).await,
                SignerBLS::Keystore(ref signer) => {
                    signer.sign_commit_boost_root(digest, cl_public_key_to_arr(pubkey.clone()))
                }
            };

            let signed_constraints = match signature {
                Ok(signature) => SignedConstraints { message, signature },
                Err(e) => {
                    error!(?e, "Failed to sign constraints");
                    let _ = response.send(Err(CommitmentError::Internal));
                    return;
                }
            };

            ApiMetrics::increment_transactions_preconfirmed(tx_type);
            self.execution.add_constraint(target_slot, signed_constraints);
        }

        // Create a commitment by signing the request
        match request.commit_and_sign(&self.commitment_signer).await {
            Ok(commitment) => {
                debug!(target_slot, elapsed = ?start.elapsed(), "Commitment signed and sent");
                response.send(Ok(commitment)).ok()
            }
            Err(err) => {
                error!(?err, "Failed to sign commitment");
                response.send(Err(CommitmentError::Internal)).ok()
            }
        };

        ApiMetrics::increment_inclusion_commitments_accepted();
    }

    /// Handle a new head event, updating the execution state.
    async fn handle_new_head_event(&mut self, head_event: HeadEvent) {
        let slot = head_event.slot;
        info!(slot, "Received new head event");

        // We use None to signal that we want to fetch the latest EL head
        if let Err(e) = self.execution.update_head(None, slot).await {
            error!(err = ?e, "Failed to update execution state head");
        }
    }

    /// Handle a commitment deadline event, submitting constraints to the Constraints client service
    /// and starting to build a local payload for the given target slot.
    async fn handle_commitment_deadline(&mut self, slot: u64) {
        debug!(slot, "Commitment deadline reached, building local block");

        let Some(template) = self.execution.get_block_template(slot) else {
            warn!("No block template found for slot {slot} when requested");
            return;
        };

        if let Err(e) = self.local_builder.build_new_local_payload(slot, template).await {
            error!(err = ?e, "Error while building local payload at deadline for slot {slot}");
        };

        // TODO: fix retry logic, and move this to separate task in the constraints client itself
        let constraints = template.signed_constraints_list.clone();
        let constraints_client = self.constraints_client.clone();
        tokio::spawn(async move {
            let max_retries = 5;
            let mut i = 0;
            while let Err(e) = constraints_client.submit_constraints(&constraints).await {
                error!(err = ?e, "Error submitting constraints to constraints client, retrying...");
                tokio::time::sleep(Duration::from_millis(100)).await;
                i += 1;
                if i >= max_retries {
                    error!("Max retries reached while submitting to Constraints client");
                    break;
                }
            }
        });
    }

    /// Handle a fetch payload request, responding with the local payload if available.
    fn handle_fetch_payload_request(&mut self, request: FetchPayloadRequest) {
        info!(slot = request.slot, "Received local payload request");

        let Some(payload_and_bid) = self.local_builder.get_cached_payload() else {
            warn!(slot = request.slot, "No local payload found");
            let _ = request.response_tx.send(None);
            return;
        };

        if let Err(e) = request.response_tx.send(Some(payload_and_bid)) {
            error!(err = ?e, "Failed to send payload and bid in response channel");
        }
    }
}

/// Pick a pubkey to sign constraints with.
///
/// Rationale:
/// - If there are no delegatee keys, try to use the validator key directly if available.
/// - If there are delegatee keys, try to use the first one that is available in the list.
fn pick_public_key(
    validator: BlsPublicKey,
    available: HashSet<BlsPublicKey>,
    delegatees: HashSet<BlsPublicKey>,
) -> Option<BlsPublicKey> {
    if delegatees.is_empty() {
        if available.contains(&validator) {
            return Some(validator);
        } else {
            return None;
        }
    } else {
        for delegatee in delegatees {
            if available.contains(&delegatee) {
                return Some(delegatee);
            }
        }
    }
    None
}
