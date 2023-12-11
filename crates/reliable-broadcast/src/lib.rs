// Copyright © Aptos Foundation
// SPDX-License-Identifier: Apache-2.0

use aptos_bounded_executor::{concurrent_map, BoundedExecutor};
use aptos_consensus_types::common::Author;
use aptos_logger::info;
use aptos_time_service::{TimeService, TimeServiceTrait};
use async_trait::async_trait;
use futures::{
    stream::{FusedStream, FuturesUnordered},
    Future, FutureExt, Stream, StreamExt,
};
use futures_channel::mpsc::Receiver;
use std::{
    collections::HashMap,
    fmt::Debug,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

pub trait RBMessage: Send + Sync + Clone {}

#[async_trait]
pub trait RBNetworkSender<Req: RBMessage, Res: RBMessage = Req>: Send + Sync {
    async fn send_rb_rpc(
        &self,
        receiver: Author,
        message: Req,
        timeout: Duration,
    ) -> anyhow::Result<Res>;
}

pub trait BroadcastStatus<Req: RBMessage, Res: RBMessage = Req>: Send + Sync + Clone {
    type Ack: Into<Res> + TryFrom<Res> + Clone;
    type Aggregated: Send;
    type Message: Into<Req> + TryFrom<Req> + Clone;

    fn add(&self, peer: Author, ack: Self::Ack) -> anyhow::Result<Option<Self::Aggregated>>;
}

pub struct ReliableBroadcast<Req: RBMessage, TBackoff, Res: RBMessage = Req> {
    validators: Vec<Author>,
    network_sender: Arc<dyn RBNetworkSender<Req, Res>>,
    backoff_policy: TBackoff,
    time_service: TimeService,
    rpc_timeout_duration: Duration,
    executor: BoundedExecutor,
}

impl<Req, TBackoff, Res> ReliableBroadcast<Req, TBackoff, Res>
where
    Req: RBMessage + 'static,
    TBackoff: Iterator<Item = Duration> + Clone + 'static,
    Res: RBMessage + 'static,
{
    pub fn new(
        validators: Vec<Author>,
        network_sender: Arc<dyn RBNetworkSender<Req, Res>>,
        backoff_policy: TBackoff,
        time_service: TimeService,
        rpc_timeout_duration: Duration,
        executor: BoundedExecutor,
    ) -> Self {
        Self {
            validators,
            network_sender,
            backoff_policy,
            time_service,
            rpc_timeout_duration,
            executor,
        }
    }

    pub fn broadcast<S: BroadcastStatus<Req, Res> + 'static>(
        &self,
        message: S::Message,
        aggregating: S,
    ) -> impl Future<Output = S::Aggregated> + 'static
    where
        <<S as BroadcastStatus<Req, Res>>::Ack as TryFrom<Res>>::Error: Debug,
    {
        let receivers: Vec<_> = self.validators.clone();
        let network_sender = self.network_sender.clone();
        let time_service = self.time_service.clone();
        let rpc_timeout_duration = self.rpc_timeout_duration;
        let mut backoff_policies: HashMap<Author, TBackoff> = self
            .validators
            .iter()
            .cloned()
            .map(|author| (author, self.backoff_policy.clone()))
            .collect();
        let executor = self.executor.clone();
        async move {
            let send_message = |receiver, message, sleep_duration: Option<Duration>| {
                let network_sender = network_sender.clone();
                let time_service = time_service.clone();
                async move {
                    if let Some(duration) = sleep_duration {
                        time_service.sleep(duration).await;
                    }
                    (
                        receiver,
                        network_sender
                            .send_rb_rpc(receiver, message, rpc_timeout_duration)
                            .await,
                    )
                }
                .boxed()
            };
            let message: Req = message.into();
            let (mut fut_tx, fut_rx) = futures_channel::mpsc::channel(receivers.len());
            for receiver in receivers {
                fut_tx
                    .try_send(send_message(receiver, message.clone(), None))
                    .expect("must enqueue");
            }
            let mut concurrent_verify_stream = concurrent_map(
                FutureWaiter::new(fut_rx),
                executor,
                move |(receiver, result)| {
                    let aggregating = aggregating.clone();
                    async move {
                        (
                            receiver,
                            result
                                .and_then(|msg| {
                                    msg.try_into().map_err(|e| anyhow::anyhow!("{:?}", e))
                                })
                                .and_then(|ack| aggregating.add(receiver, ack)),
                        )
                    }
                },
            );
            while let Some((receiver, result)) = concurrent_verify_stream.next().await {
                println!("processing result from {}", receiver);
                match result {
                    Ok(may_be_aggragated) => {
                        if let Some(aggregated) = may_be_aggragated {
                            return aggregated;
                        }
                    },
                    Err(e) => {
                        info!(error = ?e, "rpc to {} failed", receiver);

                        let backoff_strategy = backoff_policies
                            .get_mut(&receiver)
                            .expect("should be present");
                        let duration = backoff_strategy.next().expect("should produce value");
                        fut_tx
                            .try_send(send_message(receiver, message.clone(), Some(duration)))
                            .expect("must enqueue");
                    },
                }
            }
            unreachable!("Should aggregate with all responses");
        }
    }
}

// Copied from consensus/dag/dag_fetcher.rs.
// TODO: Abstract and move this to another crate and reuse with dag_fetcher.rs
struct FutureWaiter<T> {
    rx: Receiver<Pin<Box<dyn Future<Output = T> + Send>>>,
    futures: Pin<Box<FuturesUnordered<Pin<Box<dyn Future<Output = T> + Send>>>>>,
}

impl<T> FutureWaiter<T> {
    fn new(rx: Receiver<Pin<Box<dyn Future<Output = T> + Send>>>) -> Self {
        Self {
            rx,
            futures: Box::pin(FuturesUnordered::new()),
        }
    }
}

impl<T> Stream for FutureWaiter<T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.is_terminated() {
            return Poll::Ready(None)
        }

        if let Poll::Ready(Some(rx)) = self.rx.poll_next_unpin(cx) {
            self.futures.push(rx);
        }

        if !self.futures.is_empty() {
            self.futures.as_mut().poll_next(cx)
        } else {
            Poll::Pending
        }
    }
}

impl<T> FusedStream for FutureWaiter<T> {
    fn is_terminated(&self) -> bool {
        self.rx.is_terminated() && self.futures.is_empty()
    }
}

#[cfg(test)]
mod tests;
