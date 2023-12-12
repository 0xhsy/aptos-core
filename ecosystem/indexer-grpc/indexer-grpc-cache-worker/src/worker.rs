// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use crate::metrics::{
    ERROR_COUNT, LATEST_PROCESSED_VERSION as LATEST_PROCESSED_VERSION_OLD, PROCESSED_BATCH_SIZE,
    PROCESSED_LATENCY_IN_SECS, PROCESSED_VERSIONS_COUNT, WAIT_FOR_FILE_STORE_COUNTER,
};
use anyhow::{bail, ensure, Context, Result};
use aptos_indexer_grpc_utils::{
    cache_operator::CacheOperator,
    counters::{
        IndexerGrpcStep, DURATION_IN_SECS, LATEST_PROCESSED_VERSION, NUM_TRANSACTIONS_COUNT,
        TOTAL_SIZE_IN_BYTES, TRANSACTION_UNIX_TIMESTAMP,
    },
    file_store_operator::{FileStoreMetadata, FileStoreOperator},
    time_diff_since_pb_timestamp_in_secs, timestamp_to_unixtime,
};
use aptos_moving_average::MovingAverage;
use aptos_protos::internal::fullnode::v1::{
    fullnode_data_client::FullnodeDataClient, stream_status::StatusType,
    transactions_from_node_response::Response, GetTransactionsFromNodeRequest,
    TransactionsFromNodeResponse,
};
use futures::{self, StreamExt};
use prost::Message;
use tonic::transport::Channel;
use tracing::{error, info};

const FILE_STORE_VERSIONS_RESERVED: u64 = 150_000;
// Cache worker will wait if filestore is behind by
// `FILE_STORE_VERSIONS_RESERVED` versions
// Reducing this could spam the gcs metadata file so keeping it to once every 5 seconds for now (per cache worker)
const CACHE_WORKER_WAIT_FOR_FILE_STORE_IN_SECS: u64 = 5;

const SERVICE_TYPE: &str = "cache_worker";

pub struct Worker {
    cache_operator: CacheOperator<redis::aio::ConnectionManager>,
    file_store_operator: Box<dyn FileStoreOperator>,
    fullnode_grpc_client: FullnodeDataClient<Channel>,
}

/// GRPC data status enum is to identify the data frame.
/// One stream may contain multiple batches and one batch may contain multiple data chunks.
pub(crate) enum GrpcDataStatus {
    /// Ok status with processed count.
    /// Each batch may contain multiple data chunks(like 1000 transactions).
    /// These data chunks may be out of order.
    ChunkDataOk {
        start_version: u64,
        num_of_transactions: u64,
    },
    /// Init signal received with start version of current stream.
    /// No two `Init` signals will be sent in the same stream.
    StreamInit(u64),
    /// End signal received with batch end version(inclusive).
    /// Start version and its number of transactions are included for current batch.
    BatchEnd {
        start_version: u64,
        num_of_transactions: u64,
    },
}

type TransactionTypeVersion = u64;

enum CacheStartStatus {
    // Both file store and cache are present.
    Ok(TransactionTypeVersion),
    // File store is not ready.
    FileStoreIsNotReady,
    // Everything is empty.
    CacheAndFileStoreAreNotReady,
}

impl Worker {
    pub fn new(
        cache_operator: CacheOperator<redis::aio::ConnectionManager>,
        file_store_operator: Box<dyn FileStoreOperator>,
        fullnode_grpc_client: FullnodeDataClient<Channel>,
    ) -> Self {
        Self {
            cache_operator,
            file_store_operator,
            fullnode_grpc_client,
        }
    }

    /// The main loop of the worker is:
    /// 1. Determine & verify the cache start status:
    ///   * If file store and cache are not ready, start from 0, i.e., bootstrap.
    ///   * If only file store is not ready, exit early to retry.
    ///   * If both file store and cache are ready, start from file store version.
    ///     * Cache ready: chain id exists.
    ///     * File store ready: file store metadata exists.
    /// 2. Set up the grpc stream and validate the first response.
    /// 3. Process the streaming response.
    pub async fn run(&mut self) -> Result<()> {
        // Step 1.
        let cache_start_status = self.get_cache_start_status().await?;
        info!("Cache start status");
        let starting_version = match cache_start_status {
            CacheStartStatus::Ok(start_version) => start_version,
            CacheStartStatus::FileStoreIsNotReady => {
                tokio::time::sleep(std::time::Duration::from_secs(
                    CACHE_WORKER_WAIT_FOR_FILE_STORE_IN_SECS,
                ))
                .await;
                // Early return to have the worker retry.
                return Ok(());
            },
            CacheStartStatus::CacheAndFileStoreAreNotReady => {
                // Proceed to bootstrap the system.
                0
            },
        };
        // Step 2.
        let request = GetTransactionsFromNodeRequest {
            starting_version: Some(starting_version),
            ..GetTransactionsFromNodeRequest::default()
        };
        let response = match self
            .fullnode_grpc_client
            .get_transactions_from_node(request)
            .await
        {
            Ok(response) => response,
            Err(e) => {
                error!("[Indexer Cache] Streaming error: {}", e);
                ERROR_COUNT.with_label_values(&["grpc_error"]).inc();
                return Ok(());
            },
        };
        let mut response_stream = response.into_inner();

        let init_response = response_stream
            .next()
            .await
            // It's fatal for server to emit empty response. This might happen for bidirectional streaming.
            .expect("No response from fullnode on init")
            // Either response is malformed or connection is broken. Connection is just established.
            .expect("Error getting response from fullnode on init");
        self.verify_and_setup_cache(init_response, starting_version)
            .await?;

        // Step 3.
        // Infinite streaming until error happens. Broken connection is considered as expected.
        self.process_streaming_response(starting_version, response_stream)
            .await
    }

    /// Gets the cache start status.
    async fn get_cache_start_status(&mut self) -> Result<CacheStartStatus> {
        self.file_store_operator
            .verify_storage_bucket_existence()
            .await;
        let file_store_metadata_opt: Option<FileStoreMetadata> =
            self.file_store_operator.get_file_store_metadata().await;
        let cache_start_version = self.cache_operator.get_latest_version().await;
        let cache_chain_id = self.cache_operator.get_chain_id().await;
        if let Ok(cache_chain_id) = cache_chain_id {
            anyhow::ensure!(cache_start_version.is_ok(), "Cache start version is not ok");
            if let Some(file_store_metadata) = file_store_metadata_opt {
                // Case 3: start from file store version.
                anyhow::ensure!(
                    file_store_metadata.chain_id == cache_chain_id,
                    "Chain id mismatch between file store and cache."
                );
                return Ok(CacheStartStatus::Ok(file_store_metadata.version));
            } else {
                return Ok(CacheStartStatus::FileStoreIsNotReady);
            }
        }
        Ok(CacheStartStatus::CacheAndFileStoreAreNotReady)
    }

    /// Validates the first response from fullnode and setup the cache operator.
    ///   1. verify or update the chain id in the cache.
    ///   2. point the latest cache version to the file store head.
    async fn verify_and_setup_cache(
        &mut self,
        first_response: TransactionsFromNodeResponse,
        starting_version: u64,
    ) -> Result<()> {
        match first_response
            .response
            .expect("Response type does not exist.")
        {
            Response::Status(status_frame) => {
                match StatusType::try_from(status_frame.r#type).expect("Invalid status type.") {
                    StatusType::Init => {
                        ensure!(
                            status_frame.start_version == starting_version,
                            "Starting version mismatch between cache and fullnode."
                        );
                    },
                    _ => {
                        bail!("[Indexer Cache] Streaming error: first frame is not INIT signal.");
                    },
                }
            },
            _ => {
                bail!("[Indexer Cache] Streaming error: first frame is not siganl frame.");
            },
        }
        self.cache_operator
            .update_or_verify_chain_id(first_response.chain_id as u64)
            .await?;
        self.cache_operator
            .overwrite_cache_latest_version(starting_version)
            .await?;
        Ok(())
    }

    /// Infinite streaming processing. Retry if error happens; crash if fatal.
    async fn process_streaming_response(
        &mut self,
        fullnode_starting_version: u64,
        mut resp_stream: impl futures_core::Stream<Item = Result<TransactionsFromNodeResponse, tonic::Status>>
            + std::marker::Unpin,
    ) -> Result<()> {
        let mut tps_calculator = MovingAverage::new(10_000);
        let mut transaction_count = 0;
        let mut current_version = fullnode_starting_version;
        let mut starting_time = std::time::Instant::now();

        // 4. Process the streaming response.
        while let Some(received) = resp_stream.next().await {
            let received: TransactionsFromNodeResponse = match received {
                Ok(r) => r,
                Err(err) => {
                    error!("[Indexer Cache] Streaming error: {}", err);
                    ERROR_COUNT.with_label_values(&["streaming_error"]).inc();
                    return Ok(());
                },
            };

            let size_in_bytes = received.encoded_len();
            let fullnode_chain_id = received.chain_id;
            match self.process_transactions_from_node_response(received).await {
                Ok(status) => match status {
                    GrpcDataStatus::ChunkDataOk {
                        start_version,
                        num_of_transactions,
                    } => {
                        current_version += num_of_transactions;
                        transaction_count += num_of_transactions;
                        tps_calculator.tick_now(num_of_transactions);

                        PROCESSED_VERSIONS_COUNT.inc_by(num_of_transactions);
                        // TODO: Reasses whether this metric useful
                        LATEST_PROCESSED_VERSION_OLD.set(current_version as i64);
                        PROCESSED_BATCH_SIZE.set(num_of_transactions as i64);
                        info!(
                            start_version = start_version,
                            num_of_transactions = num_of_transactions,
                            "[Indexer Cache] Data chunk received.",
                        );
                    },
                    GrpcDataStatus::StreamInit(new_version) => {
                        ERROR_COUNT.with_label_values(&["data_init_twice"]).inc();
                        bail!(format!(
                            "[Indexer Cache] Init signal received twice, version: {}.",
                            new_version
                        ));
                    },
                    GrpcDataStatus::BatchEnd {
                        start_version,
                        num_of_transactions,
                    } => {
                        info!(
                            start_version = start_version,
                            num_of_transactions = num_of_transactions,
                            "[Indexer Cache] End signal received for current batch.",
                        );
                        if current_version != start_version + num_of_transactions {
                            error!(
                                current_version = current_version,
                                actual_current_version = start_version + num_of_transactions,
                                "[Indexer Cache] End signal received with wrong version."
                            );
                            ERROR_COUNT
                                .with_label_values(&["data_end_wrong_version"])
                                .inc();
                            break;
                        }
                        self.cache_operator
                            .update_cache_latest_version(transaction_count, current_version)
                            .await
                            .context("Failed to update the latest version in the cache")?;
                        transaction_count = 0;
                        info!(
                            current_version = current_version,
                            chain_id = fullnode_chain_id,
                            tps = (tps_calculator.avg() * 1000.0) as u64,
                            "[Indexer Cache] Successfully process current batch."
                        );
                        LATEST_PROCESSED_VERSION
                            .with_label_values(&[
                                SERVICE_TYPE,
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_step(),
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_label(),
                            ])
                            .set((start_version + num_of_transactions - 1) as i64);
                        NUM_TRANSACTIONS_COUNT
                            .with_label_values(&[
                                SERVICE_TYPE,
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_step(),
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_label(),
                            ])
                            .set(num_of_transactions as i64);
                        TOTAL_SIZE_IN_BYTES
                            .with_label_values(&[
                                SERVICE_TYPE,
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_step(),
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_label(),
                            ])
                            .set(size_in_bytes as i64);
                        DURATION_IN_SECS
                            .with_label_values(&[
                                SERVICE_TYPE,
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_step(),
                                IndexerGrpcStep::CacheWorkerBatchProcessed.get_label(),
                            ])
                            .set(starting_time.elapsed().as_secs() as f64);
                        starting_time = std::time::Instant::now();
                    },
                },
                Err(e) => {
                    error!(
                        "[Indexer Cache] Process transactions from fullnode failed: {}",
                        e
                    );
                    ERROR_COUNT.with_label_values(&["response_error"]).inc();
                    break;
                },
            }

            // Check if the file store isn't too far away
            loop {
                // TODO: similar todo as above, ensure that the filestore metadata is already there to avoid a conflict
                let file_store_version = self
                    .cache_operator
                    .get_file_store_latest_version()
                    .await
                    .unwrap();
                if file_store_version + FILE_STORE_VERSIONS_RESERVED < current_version {
                    tokio::time::sleep(std::time::Duration::from_secs(
                        CACHE_WORKER_WAIT_FOR_FILE_STORE_IN_SECS,
                    ))
                    .await;
                    tracing::warn!(
                        current_version = current_version,
                        file_store_version = file_store_version,
                        "[Indexer Cache] File store version is behind current version too much."
                    );
                    WAIT_FOR_FILE_STORE_COUNTER.inc();
                } else {
                    // File store is up to date, continue cache update.
                    break;
                }
            }
        }

        // It is expected that we get to this point, the upstream server disconnects
        // clients after 5 minutes.
        Ok(())
    }

    async fn process_transactions_from_node_response(
        &mut self,
        response: TransactionsFromNodeResponse,
    ) -> Result<GrpcDataStatus> {
        match response.response.unwrap() {
            Response::Status(status) => {
                match StatusType::try_from(status.r#type)
                    .expect("[Indexer Cache] Invalid status type.")
                {
                    StatusType::Init => Ok(GrpcDataStatus::StreamInit(status.start_version)),
                    StatusType::BatchEnd => {
                        let start_version = status.start_version;
                        let num_of_transactions = status
                            .end_version
                            .expect("TransactionsFromNodeResponse status end_version is None")
                            - start_version
                            + 1;
                        Ok(GrpcDataStatus::BatchEnd {
                            start_version,
                            num_of_transactions,
                        })
                    },
                    StatusType::Unspecified => unreachable!("Unspecified status type."),
                }
            },
            Response::Data(data) => {
                let transaction_len = data.transactions.len();
                let first_transaction = data
                    .transactions
                    .first()
                    .context("There were unexpectedly no transactions in the response")?;
                let last_transaction = data
                    .transactions
                    .last()
                    .context("There were unexpectedly no transactions in the response")?;
                let start_version = first_transaction.version;
                let last_transaction_pb_timestamp = last_transaction.timestamp.clone();
                let transactions = data
                    .transactions
                    .into_iter()
                    .map(|tx| {
                        let timestamp_in_seconds = match tx.timestamp {
                            Some(ref timestamp) => timestamp.seconds as u64,
                            None => 0,
                        };
                        let mut encoded_proto_data = vec![];
                        tx.encode(&mut encoded_proto_data)
                            .context("Encode transaction failed.")?;
                        let base64_encoded_proto_data = base64::encode(encoded_proto_data);
                        Ok((tx.version, base64_encoded_proto_data, timestamp_in_seconds))
                    })
                    .collect::<Result<Vec<(u64, String, u64)>>>()?;

                // Push to cache.
                match self
                    .cache_operator
                    .update_cache_transactions(transactions)
                    .await
                {
                    Ok(_) => {},
                    Err(e) => {
                        ERROR_COUNT
                            .with_label_values(&["failed_to_update_cache_version"])
                            .inc();
                        bail!("Update cache with version failed: {}", e);
                    },
                }
                if let Some(ref txn_time) = last_transaction_pb_timestamp {
                    PROCESSED_LATENCY_IN_SECS.set(time_diff_since_pb_timestamp_in_secs(txn_time));
                    TRANSACTION_UNIX_TIMESTAMP
                        .with_label_values(&[
                            SERVICE_TYPE,
                            IndexerGrpcStep::CacheWorkerTxnsProcessed.get_step(),
                            IndexerGrpcStep::CacheWorkerTxnsProcessed.get_label(),
                        ])
                        .set(timestamp_to_unixtime(txn_time));
                }
                Ok(GrpcDataStatus::ChunkDataOk {
                    start_version,
                    num_of_transactions: transaction_len as u64,
                })
            },
        }
    }
}
