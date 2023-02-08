//! # Burn Totals
//! This module sums total burn. Limited, and growing time frames.
//!
//! ## Growing time frames
//! Get the last valid sum. If one exists, continue, otherwise, start from zero. Continue with
//! addition.
//!
//! ### GetLastValidSum
//! Check the hash of the last sum exists in the blocks table. If it does, return the sum, if it
//! does not, iterate backwards through the sums, dropping as we go, until one is found with a hash in the blocks
//! table. If none is found, report none exists.
//!
//! ### Addition
//! Gather the burn of each block after the last valid sum. Sum them iteratively and remember the
//! last one hundred sums calculated. Add them to the sums table.
//!
//! ## Limited time frames
//! Get the last valid sum. If one exists, continue, otherwise, start from zero. Continue with
//! expiration, then addition.
//!
//! ### GetLastValidSum
//! Check the hash of the last sum exists in the blocks table. If it does, return the sum, if it
//! does not, iterate backwards through the in-frame blocks, dropping as we go, whilst subtracting from the sum, until one is found with a hash in the blocks
//! table. If none is found, report none exists.
//!
//! ### Expiration
//! Iterate forward through the in-frame blocks, dropping as we go, whilst subtracting from the
//! sum, until one is found which is timestamped after NOW - TIME_FRAME_INTERVAL.
//!
//! ### Addition
//! Gather the burn of each block after the last valid sum. Remember each in-frame block, and
//! update the sum.
//!
//! ## Table schema
//! time_frame,block_number,block_hash,timestamp,burn,sum

mod store;

use std::cmp::Ordering;

use chrono::{DateTime, Utc};
use futures::join;
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use tracing::debug;

use crate::{
    burn_rates::BurnRates,
    burn_sums::store::BurnSumStore,
    caching::{self, CacheKey},
    execution_chain::{BlockNumber, BlockRange, BlockStore, ExecutionNodeBlock},
    performance::TimedExt,
    time_frames::{GrowingTimeFrame, LimitedTimeFrame, TimeFrame},
    units::{EthNewtype, UsdNewtype, WeiNewtype},
};

#[derive(Debug, PartialEq)]
struct WeiUsdAmount {
    wei: WeiNewtype,
    usd: UsdNewtype,
}

#[derive(Debug, PartialEq, Serialize)]
struct EthUsdAmount {
    pub eth: EthNewtype,
    pub usd: UsdNewtype,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct BurnSums {
    pub since_merge: EthUsdAmount,
    pub since_burn: EthUsdAmount,
    pub d1: EthUsdAmount,
    pub d30: EthUsdAmount,
    pub d7: EthUsdAmount,
    pub h1: EthUsdAmount,
    pub m5: EthUsdAmount,
}

#[derive(Debug)]
pub struct BurnSumRecord {
    last_included_block_hash: String,
    first_included_block_number: BlockNumber,
    last_included_block_number: BlockNumber,
    sum_wei: WeiNewtype,
    sum_usd: UsdNewtype,
    time_frame: TimeFrame,
    timestamp: DateTime<Utc>,
}

pub async fn on_rollback(connection: &mut PgConnection, block_number_gte: &BlockNumber) {
    BurnSumStore::delete_new_sums_tx(connection, block_number_gte).await;
}

async fn expired_burn_from(
    block_store: &BlockStore<'_>,
    burn_sum_store: &BurnSumStore<'_>,
    last_burn_sum: &BurnSumRecord,
    block: &ExecutionNodeBlock,
    limited_time_frame: &LimitedTimeFrame,
) -> Option<(BlockNumber, WeiNewtype, UsdNewtype)> {
    // The first included block for the next sum may have jumped forward zero or
    // more blocks. Meaning zero or more blocks are now considered expired but
    // still included for this limited time frame sum.
    let age_limit = block.timestamp - limited_time_frame.duration();
    let first_included_block_number = block_store
        .first_number_after_or_at(&age_limit)
        .await
        .expect(
            "failed to get first block number after or at block.timestamp - limited_time_frame",
        );

    match first_included_block_number.cmp(&last_burn_sum.first_included_block_number) {
        Ordering::Less => {
            // Current block number should be > the last sum's block number. So the
            // first included block should be greater or equal too, yet the first included
            // block number for the current block is smaller. This should be
            // impossible.
            panic!("first included block number for current block is smaller than the last sum's first included block number");
        }
        Ordering::Equal => {
            // The last sum included the same blocks as the current block, so the
            // new sum is the same as the last sum.
            debug!("first included block number is the same for the current block and the last sum, no expired burn");
            None
        }
        Ordering::Greater => {
            let expired_block_range = BlockRange::new(
                last_burn_sum.first_included_block_number,
                first_included_block_number - 1,
            );

            let (expired_included_burn_wei, expired_included_burn_usd) = burn_sum_store
                .burn_sum_from_block_range(&expired_block_range)
                .await;

            debug!(%expired_block_range, %expired_included_burn_wei, %expired_included_burn_usd, %limited_time_frame, "subtracting expired burn");

            Some((
                first_included_block_number,
                expired_included_burn_wei,
                expired_included_burn_usd,
            ))
        }
    }
}

async fn calc_new_burn_sum_record_from_scratch(
    burn_sum_store: &BurnSumStore<'_>,
    block: &ExecutionNodeBlock,
    time_frame: &TimeFrame,
) -> BurnSumRecord {
    debug!(%block.number, %block.hash, %time_frame, "calculating new burn sum record from scratch");
    let range = BlockRange::from_last_plus_time_frame(&block.number, time_frame);
    let (sum_wei, sum_usd) = burn_sum_store.burn_sum_from_block_range(&range).await;
    BurnSumRecord {
        first_included_block_number: range.start,
        last_included_block_hash: block.hash.clone(),
        last_included_block_number: range.end,
        sum_wei,
        sum_usd,
        time_frame: *time_frame,
        timestamp: block.timestamp,
    }
}

async fn calc_new_burn_sum_record_from_last(
    block_store: &BlockStore<'_>,
    burn_sum_store: &BurnSumStore<'_>,
    last_burn_sum: &BurnSumRecord,
    block: &ExecutionNodeBlock,
    time_frame: &TimeFrame,
) -> BurnSumRecord {
    debug!(%block.number, %block.hash, %time_frame, "calculating new burn sum record from last");
    let new_burn_range =
        BlockRange::new(last_burn_sum.last_included_block_number + 1, block.number);
    let (new_burn_wei, new_burn_usd) = burn_sum_store
        .burn_sum_from_block_range(&new_burn_range)
        .await;

    let expired_burn_sum = match time_frame {
        TimeFrame::Limited(limited_time_frame) => {
            expired_burn_from(
                block_store,
                burn_sum_store,
                last_burn_sum,
                block,
                limited_time_frame,
            )
            .await
        }
        TimeFrame::Growing(_) => None,
    };

    let (sum_wei, sum_usd) = match expired_burn_sum {
        Some((_, expired_burn_sum_wei, expired_burn_sum_usd)) => {
            let sum_wei = new_burn_wei - expired_burn_sum_wei + last_burn_sum.sum_wei;
            let sum_usd = new_burn_usd - expired_burn_sum_usd + last_burn_sum.sum_usd;
            (sum_wei, sum_usd)
        }
        None => {
            let sum_wei = new_burn_wei + last_burn_sum.sum_wei;
            let sum_usd = new_burn_usd + last_burn_sum.sum_usd;
            (sum_wei, sum_usd)
        }
    };

    let first_included_block_number = expired_burn_sum
        .map(|(first_included_block_number, _, _)| first_included_block_number)
        // If there is no expired burn, the first included did not change.
        .unwrap_or(last_burn_sum.first_included_block_number);

    BurnSumRecord {
        first_included_block_number,
        last_included_block_hash: block.hash.clone(),
        last_included_block_number: new_burn_range.end,
        sum_wei,
        sum_usd,
        time_frame: *time_frame,
        timestamp: block.timestamp,
    }
}

async fn calc_new_burn_sum_record(
    block_store: &BlockStore<'_>,
    burn_sum_store: &BurnSumStore<'_>,
    block: &ExecutionNodeBlock,
    time_frame: &TimeFrame,
) -> BurnSumRecord {
    match burn_sum_store.last_burn_sum(time_frame).await {
        Some(last_burn_sum) => {
            calc_new_burn_sum_record_from_last(
                block_store,
                burn_sum_store,
                &last_burn_sum,
                block,
                time_frame,
            )
            .await
        }
        None => calc_new_burn_sum_record_from_scratch(burn_sum_store, block, time_frame).await,
    }
}

pub async fn on_new_block(db_pool: &PgPool, block: &ExecutionNodeBlock) {
    use GrowingTimeFrame::*;
    use LimitedTimeFrame::*;
    use TimeFrame::*;

    let block_store = BlockStore::new(db_pool);
    let burn_sum_store = BurnSumStore::new(db_pool);

    let (since_burn, since_merge, d30, d7, d1, h1, m5) = join!(
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Growing(SinceBurn))
            .timed("calc_new_burn_sum_record_since_burn"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Growing(SinceMerge))
            .timed("calc_new_burn_sum_record_since_merge"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Limited(Day30))
            .timed("calc_new_burn_sum_record_day30"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Limited(Day7))
            .timed("calc_new_burn_sum_record_day7"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Limited(Day1))
            .timed("calc_new_burn_sum_record_day1"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Limited(Hour1))
            .timed("calc_new_burn_sum_record_hour1"),
        calc_new_burn_sum_record(&block_store, &burn_sum_store, block, &Limited(Minute5))
            .timed("calc_new_burn_sum_record_minute5")
    );

    let burn_sums = [&since_burn, &since_merge, &d30, &d7, &d1, &h1, &m5];
    burn_sum_store.store_burn_sums(burn_sums).await;

    // Drop old sums.
    burn_sum_store.delete_old_sums(block.number).await;

    let burn_sums = BurnSums {
        since_burn: EthUsdAmount {
            eth: since_burn.sum_wei.into(),
            usd: since_burn.sum_usd,
        },
        since_merge: EthUsdAmount {
            eth: since_merge.sum_wei.into(),
            usd: since_merge.sum_usd,
        },
        d30: EthUsdAmount {
            eth: d30.sum_wei.into(),
            usd: d30.sum_usd,
        },
        d7: EthUsdAmount {
            eth: d7.sum_wei.into(),
            usd: d7.sum_usd,
        },
        d1: EthUsdAmount {
            eth: d1.sum_wei.into(),
            usd: d1.sum_usd,
        },
        h1: EthUsdAmount {
            eth: h1.sum_wei.into(),
            usd: h1.sum_usd,
        },
        m5: EthUsdAmount {
            eth: m5.sum_wei.into(),
            usd: m5.sum_usd,
        },
    };

    debug!("calculated new burn sums");

    let burn_rates: BurnRates = (&burn_sums).into();

    debug!("calculated new burn rates");

    caching::update_and_publish(db_pool, &CacheKey::BurnSums, burn_sums)
        .await
        .unwrap();

    caching::update_and_publish(db_pool, &CacheKey::BurnRates, burn_rates)
        .await
        .unwrap();
}
