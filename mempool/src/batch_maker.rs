use crate::mempool::MempoolMessage;
use crate::quorum_waiter::QuorumWaiterMessage;
use bytes::Bytes;
#[cfg(feature = "benchmark")]
use crypto::Digest;
use crypto::PublicKey;
#[cfg(feature = "benchmark")]
use ed25519_dalek::{Digest as _, Sha512};
#[cfg(feature = "benchmark")]
use log::info;
use network::ReliableSender;
use types::CBlock;
#[cfg(feature = "benchmark")]
use std::convert::TryInto as _;
use std::net::SocketAddr;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::time::{sleep, Duration};

#[cfg(test)]
#[path = "tests/batch_maker_tests.rs"]
pub mod batch_maker_tests;

pub type Transaction = Vec<u8>;
pub type Batch = Vec<Transaction>;  

/// Assemble clients transactions into batches.
pub struct BatchMaker {
    /// The preferred batch size (in bytes).
    _batch_size: usize,
    /// The maximum delay after which to seal the batch (in ms).
    max_batch_delay: u64,
    /// Channel to receive transactions (cblock) from the network.
    // rx_transaction: Receiver<Transaction>,
    rx_transaction: Receiver<CBlock>,
    /// Output channel to deliver sealed batches to the `QuorumWaiter`.
    tx_message: Sender<QuorumWaiterMessage>,
    /// The network addresses of the other mempools.
    mempool_addresses: Vec<(PublicKey, SocketAddr)>,
    /// Holds the current batch.
    current_batch: Batch,
    /// Holds the size of the current batch (in bytes).
    current_batch_size: usize,
    /// A network sender to broadcast the batches to the other mempools.
    network: ReliableSender,
}

impl BatchMaker {
    pub fn spawn(
        _batch_size: usize,
        max_batch_delay: u64,
        rx_transaction: Receiver<CBlock>,
        tx_message: Sender<QuorumWaiterMessage>,
        mempool_addresses: Vec<(PublicKey, SocketAddr)>,
    ) {
        tokio::spawn(async move {
            Self {
                _batch_size,
                max_batch_delay,
                rx_transaction,
                tx_message,
                mempool_addresses,
                current_batch: Batch::with_capacity(_batch_size * 2),
                current_batch_size: 0,
                network: ReliableSender::new(),
            }
            .run()
            .await;
        });
    }

    /// Main loop receiving incoming transactions and creating batches.
    async fn run(&mut self) {
        let _timer = sleep(Duration::from_millis(self.max_batch_delay));
        tokio::pin!(_timer);

        loop {
            tokio::select! {
                // Receive transaction (CBlock).
                Some(transaction) = self.rx_transaction.recv() => {
                    // Broadcast to other peers and send it to consensus for further processing 
                    #[cfg(feature = "benchmark")]
                    info!("Successfully receive cblock");
                    let serialize_cblock = bincode::serialize(&transaction)
                        .expect("Fail to serialize transaction");
                    self.current_batch.push(serialize_cblock);
                    self.seal().await;
                },
            }

            // Give the change to schedule other tasks.
            tokio::task::yield_now().await;
        }
    }

    /// Seal and broadcast the current batch.
    async fn seal(&mut self) {
        #[cfg(feature = "benchmark")]
        let size = self.current_batch_size;

        // Look for sample txs (they all start with 0) and gather their txs id (the next 8 bytes).
        #[cfg(feature = "benchmark")]
        let tx_ids: Vec<_> = self
            .current_batch
            .iter()
            .filter(|tx| tx[0] == 0u8 && tx.len() > 8)
            .filter_map(|tx| tx[1..9].try_into().ok())
            .collect();

        // Serialize the batch.
        self.current_batch_size = 0;
        let batch: Vec<_> = self.current_batch.drain(..).collect();
        // ARETE: replace 'batch' with 'Transaction'
        // Only send the first transaction of the batch
        let tx = batch[0].clone();
        let cblock: CBlock = bincode::deserialize(&tx)
            .expect("fail to deserialize the CBlock");
        let message = MempoolMessage::CBlock(cblock);
        let serialized = bincode::serialize(&message).expect("Failed to serialize our own batch");

        #[cfg(feature = "benchmark")]
        {
            // NOTE: This is one extra hash that is only needed to print the following log entries.
            let digest = Digest(
                Sha512::digest(&serialized).as_slice()[..32]
                    .try_into()
                    .unwrap(),
            );

            for id in tx_ids {
                // NOTE: This log entry is used to compute performance.
                info!(
                    "Batch {:?} contains sample tx {}",
                    digest,
                    u64::from_be_bytes(id)
                );
            }

            // NOTE: This log entry is used to compute performance.
            info!("Batch {:?} contains {} B", digest, size);
        }

        // Broadcast the batch through the network.
        // A batch is a Vec<Transaction>
        let (names, addresses): (Vec<_>, _) = self.mempool_addresses.iter().cloned().unzip();
        let bytes = Bytes::from(serialized.clone());
        let handlers = self.network.broadcast(addresses, bytes).await;

        // Send the batch through the deliver channel for further processing.
        self.tx_message
            .send(QuorumWaiterMessage {
                batch: serialized,
                handlers: names.into_iter().zip(handlers.into_iter()).collect(),
            })
            .await
            .expect("Failed to deliver batch");
    }
}
