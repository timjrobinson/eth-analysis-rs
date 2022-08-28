//! Listens to a node, keeps a consistent view of the chain in our own DB and makes sure to
//! propagate updates to this view as needed by dependent modules.
//!
//! We have an existing service handling block syncing, written in TypeScript. A lot of "syncing
//! from a node" code has been rewritten for this newer service. Taking learnings from the existing
//! code, adding more tests, and improving designs. This side should slowly take over more
//! responsibilities.

use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use sqlx::PgPool;
use std::cmp::Ordering;
use std::collections::VecDeque;
use std::iter::Iterator;
use std::sync::Arc;
use std::sync::Mutex;

use super::eth_prices;
use super::node::BlockNumber;
use super::node::Head;
use crate::config;
use crate::execution_chain;
use crate::execution_chain::block_store::BlockStore;
use crate::execution_chain::merge_estimate;
use crate::execution_chain::ExecutionNode;
use crate::performance::TimedExt;

async fn rollback_numbers(block_store: &mut BlockStore<'_>, greater_than_or_equal: &BlockNumber) {
    tracing::debug!("rolling back data based on numbers gte {greater_than_or_equal}");
    block_store.delete_blocks(&greater_than_or_equal).await;
}

async fn sync_by_hash(
    block_store: &mut BlockStore<'_>,
    execution_node: &mut ExecutionNode,
    executor: &PgPool,
    hash: &str,
) {
    let block = execution_node
        .get_block_by_hash(hash)
        .await
        .expect("block not to disappear between deciding to add it and adding it");

    let mut connection = executor.acquire().await.unwrap();
    let eth_price = eth_prices::get_eth_price_by_block(&mut *connection, &block)
        .await
        .expect("eth price close to block to be available");

    // Between the time we received the head event and requested a header for the given
    // block_root the block may have disappeared. Right now we panic, we could do better.
    block_store.store_block(&block, eth_price).await;

    let is_synced = execution_node.get_latest_block().await.hash == hash;
    // Some computations can be skipped, others should be ran, and rolled back for every change in
    // the chain of blocks we've assembled. These are the ones that are skippable, and so skipped
    // until we're in-sync with the chain again.
    if is_synced {
        tracing::debug!("we're synced, running on_new_head for skippables");
        merge_estimate::on_new_head(executor, &block).await;
    }
}

enum NextStep {
    HandleGap,
    HandleHeadFork,
    AddToExisting,
}

async fn get_next_step(block_store: &mut BlockStore<'_>, head: &Head) -> NextStep {
    // Between the time we received the head event and requested a header for the given
    // block_root the block may have disappeared. Right now we panic, we could do better.
    let is_parent_known = block_store
        .get_is_parent_hash_known(&head.parent_hash)
        .await;

    if !is_parent_known {
        return NextStep::HandleGap;
    }

    let is_fork_block = block_store.get_is_number_known(&head.number).await;

    if is_fork_block {
        return NextStep::HandleHeadFork;
    }

    NextStep::AddToExisting
}

async fn sync_head(
    store: &mut BlockStore<'_>,
    execution_node: &mut ExecutionNode,
    db_pool: &PgPool,
    heads_queue: HeadsQueue,
    head_to_sync: HeadToSync,
) {
    let head_event = match head_to_sync {
        HeadToSync::Fetched(head) => head,
        HeadToSync::Refetch(block_number) => execution_node
            .get_block_by_number(&block_number)
            .await
            .expect("chain not to get shorter since scheduling refetch head sync")
            .into(),
    };

    tracing::debug!("sync head from number: {}", head_event.number);

    match get_next_step(store, &head_event).await {
        NextStep::HandleGap => {
            tracing::warn!("parent of block at block_number {} is missing, dropping min(our last block.block_number, new block.block_number) and queueing all blocks gte the received block, block: {}", head_event.number, head_event.hash);

            let last_block_number = store
                .get_last_block_number()
                .await
                .expect("at least one block to be synced before rolling back");

            // Head number may be lower than our last synced. Roll back gte lowest of the two.
            let lowest_number = last_block_number.min(head_event.number);

            rollback_numbers(store, &lowest_number).await;

            for number in (lowest_number..=head_event.number).rev() {
                tracing::debug!("queueing {number} for sync after dropping");
                heads_queue
                    .lock()
                    .unwrap()
                    .push_front(HeadToSync::Refetch(number));
            }
        }
        NextStep::HandleHeadFork => {
            tracing::info!(
                "block at number {} creates a fork, rolling back our last block - {}",
                head_event.number,
                head_event.hash
            );

            rollback_numbers(store, &head_event.number).await;

            heads_queue
                .lock()
                .unwrap()
                .push_front(HeadToSync::Fetched(head_event))
        }
        NextStep::AddToExisting => {
            sync_by_hash(store, execution_node, db_pool, &head_event.hash)
                .timed("sync numbers")
                .await;
        }
    }
}

#[derive(Clone)]
pub struct BlockRange {
    greater_than_or_equal: BlockNumber,
    less_than_or_equal: BlockNumber,
}

impl BlockRange {
    pub fn new(greater_than_or_equal: u32, less_than_or_equal: u32) -> Self {
        if greater_than_or_equal > less_than_or_equal {
            panic!("tried to create slot range with negative range")
        }

        Self {
            greater_than_or_equal,
            less_than_or_equal,
        }
    }
}

pub struct BlockRangeIntoIterator {
    block_range: BlockRange,
    index: usize,
}

impl IntoIterator for BlockRange {
    type Item = u32;
    type IntoIter = BlockRangeIntoIterator;

    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter {
            block_range: self,
            index: 0,
        }
    }
}

impl Iterator for BlockRangeIntoIterator {
    type Item = BlockNumber;

    fn next(&mut self) -> Option<Self::Item> {
        match (self.block_range.greater_than_or_equal as usize + self.index)
            .cmp(&(self.block_range.less_than_or_equal as usize))
        {
            Ordering::Less => {
                let current = self.block_range.greater_than_or_equal + self.index as u32;
                self.index = self.index + 1;
                Some(current)
            }
            Ordering::Equal => {
                let current = self.block_range.greater_than_or_equal + self.index as u32;
                self.index = self.index + 1;
                Some(current)
            }
            Ordering::Greater => None,
        }
    }
}

fn get_historic_stream(block_range: BlockRange) -> impl Stream<Item = Head> {
    let (mut tx, rx) = futures::channel::mpsc::channel(10);

    tokio::spawn(async move {
        let mut execution_node = ExecutionNode::connect().await;
        for block_number in block_range.into_iter() {
            let block = execution_node
                .get_block_by_number(&block_number)
                .await
                .unwrap();
            tx.send(block.into()).await.unwrap();
        }
    });

    rx
}

async fn stream_heads_from(gte_slot: BlockNumber) -> impl Stream<Item = Head> {
    tracing::debug!("streaming heads from {gte_slot}");

    let mut execution_node = ExecutionNode::connect().await;
    let last_block_on_start = execution_node.get_latest_block().await;
    tracing::debug!("last block on chain: {}", &last_block_on_start.number);

    // We stream heads as requested until caught up with the chain and then pass heads as they come
    // in from our node. The only way to be sure how high we should stream, is to wait for the
    // first head from the node to come in. We don't want to wait. So ask for the latest head, take
    // this as the max, and immediately start listening for new heads. Running the small risk the
    // chain has advanced between these two calls.
    let heads_stream = execution_chain::stream_new_heads();

    let block_range = BlockRange::new(gte_slot, last_block_on_start.number - 1);

    let historic_heads_stream = get_historic_stream(block_range);

    historic_heads_stream.chain(heads_stream)
}

async fn stream_heads_from_last(db: &PgPool) -> impl Stream<Item = Head> {
    let mut connection = db.acquire().await.unwrap();
    let mut block_store = BlockStore::new(&mut *connection);
    let next_block_to_sync = block_store
        .get_last_block_number()
        .await
        .map_or(15253306, |number| number + 1);
    stream_heads_from(next_block_to_sync).await
}

#[derive(Clone, Debug)]
enum HeadToSync {
    Fetched(Head),
    Refetch(BlockNumber),
}
type HeadsQueue = Arc<Mutex<VecDeque<HeadToSync>>>;

pub async fn sync_blocks() {
    tracing_subscriber::fmt::init();

    tracing::info!("syncing execution blocks");

    let db_pool = PgPool::connect(&config::get_db_url_with_name("sync-execution-blocks"))
        .await
        .unwrap();

    sqlx::migrate!().run(&db_pool).await.unwrap();

    let mut execution_node = ExecutionNode::connect().await;

    let mut connection = db_pool.acquire().await.unwrap();
    let mut block_store = BlockStore::new(&mut connection);

    let mut heads_stream = stream_heads_from_last(&db_pool).await;

    let heads_queue: HeadsQueue = Arc::new(Mutex::new(VecDeque::new()));

    while let Some(head_event) = heads_stream.next().await {
        heads_queue
            .lock()
            .unwrap()
            .push_back(HeadToSync::Fetched(head_event));

        // Work through the heads queue until it's empty and we're ready to move the next head from
        // the stream to the queue.
        loop {
            let next_head = { heads_queue.lock().unwrap().pop_front() };
            match next_head {
                None => {
                    // Continue syncing heads from the stream.
                    break;
                }
                Some(next_head) => {
                    // Because we may encounter rollbacks, this step may add more heads to sync to
                    // the front of the queue.
                    sync_head(
                        &mut block_store,
                        &mut execution_node,
                        &db_pool,
                        heads_queue.clone(),
                        next_head,
                    )
                    .timed("sync head")
                    .await;
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sqlx::Acquire;

    use crate::db_testing::get_test_db;
    use crate::execution_chain::node::ExecutionNodeBlock;

    use super::*;

    #[tokio::test]
    async fn rollback_last_first_test() {
        let mut db = get_test_db().await;
        let mut connection = db.acquire().await.unwrap();
        let mut block_store = BlockStore::new(&mut connection);

        block_store
            .store_block(
                &ExecutionNodeBlock {
                    base_fee_per_gas: 0,
                    difficulty: 0,
                    gas_used: 0,
                    hash: "0xtest".to_string(),
                    number: 0,
                    parent_hash: "0xparent".to_string(),
                    timestamp: Utc::now(),
                    total_difficulty: 0,
                },
                0.0,
            )
            .await;

        block_store
            .store_block(
                &ExecutionNodeBlock {
                    base_fee_per_gas: 0,
                    difficulty: 0,
                    gas_used: 0,
                    hash: "0xhash2".to_string(),
                    number: 1,
                    parent_hash: "0xhash".to_string(),
                    timestamp: Utc::now(),
                    total_difficulty: 0,
                },
                0.0,
            )
            .await;

        rollback_numbers(&mut block_store, &0).await;

        // This should blow up if the order is backwards but its not obvious how. Consider using
        // mockall to create a mock instance of block_store so we can observe whether
        // rollback_numbers is calling it correctly.
    }

    #[test]
    fn block_range_iterable_test() {
        let range = (BlockRange::new(1, 4)).into_iter().collect::<Vec<u32>>();
        assert_eq!(range, vec![1, 2, 3, 4]);
    }
}
