use std::collections::HashSet;

use futures::prelude::*;
use serde::Serialize;
use sqlx::postgres::PgPoolOptions;

use crate::{config, execution_node::ExecutionNode};

const SUPPLY_DELTA_BUFFER_SIZE: usize = 10_000;

pub async fn write_deltas() {
    tracing_subscriber::fmt::init();

    let timestamp = crate::time::get_timestamp();

    tracing::info!("writing supply deltas {timestamp}");

    let mut supply_deltas_rx =
        crate::execution_node::stream_supply_delta_chunks(0, SUPPLY_DELTA_BUFFER_SIZE);

    let mut progress = pit_wall::Progress::new("write supply deltas", 15_000_000);

    let file_path = format!("supply_deltas_{}.csv", timestamp);

    let mut csv_writer = csv::Writer::from_path(&file_path).unwrap();

    while let Some(supply_deltas) = supply_deltas_rx.next().await {
        for supply_delta in supply_deltas {
            csv_writer.serialize(supply_delta).unwrap();
        }

        progress.inc_work_done_by(SUPPLY_DELTA_BUFFER_SIZE.try_into().unwrap());
        tracing::debug!("{}", progress.get_progress_string());
    }

    // A CSV writer maintains an internal buffer, so it's important
    // to flush the buffer when you're done.
    csv_writer.flush().unwrap();
}

pub async fn sync_deltas() {
    tracing_subscriber::fmt::init();

    tracing::info!("syncing supply deltas");

    let pool = PgPoolOptions::new()
        .max_connections(5)
        .connect(&config::get_db_url())
        .await
        .unwrap();

    sqlx::migrate!().run(&pool).await.unwrap();

    let mut execution_node = ExecutionNode::connect().await;
    let latest_block = execution_node.get_latest_block().await;

    dbg!(latest_block);

    let mut new_heads_rx = crate::execution_node::stream_new_heads();

    while let Some(new_head) = new_heads_rx.next().await {
        // let _latest_stored_block = crate::execution_chain::get_latest_block(&pool).await;
        dbg!(new_head);
    }
}

#[derive(Serialize)]
struct SupplyDeltaLog {
    block_number: u32,
    hash: String,
    is_duplicate_number: bool,
    is_jumping_ahead: bool,
    parent_hash: String,
    received_at: String,
}

pub async fn write_deltas_log() {
    tracing_subscriber::fmt::init();

    let timestamp = chrono::offset::Utc::now().timestamp();

    tracing::info!("writing supply delta log {timestamp}");

    let mut execution_node = ExecutionNode::connect().await;
    let latest_block = execution_node.get_latest_block().await;

    let mut supply_deltas_rx = crate::execution_node::stream_supply_deltas(latest_block.number);

    let file_path = format!("supply_deltas_log_{}.csv", timestamp);

    let mut csv_writer = csv::Writer::from_path(&file_path).unwrap();

    let mut seen_block_heights = HashSet::<u32>::new();
    let mut seen_block_hashes = HashSet::<String>::new();

    while let Some(supply_delta) = supply_deltas_rx.next().await {
        let is_duplicate_number = seen_block_heights.contains(&supply_delta.block_number);
        let is_jumping_ahead =
            !seen_block_hashes.is_empty() && !seen_block_hashes.contains(&supply_delta.parent_hash);

        seen_block_heights.insert(supply_delta.block_number.clone());
        seen_block_hashes.insert(supply_delta.hash.clone());

        let supply_delta_log = SupplyDeltaLog {
            block_number: supply_delta.block_number,
            hash: supply_delta.hash,
            parent_hash: supply_delta.parent_hash,
            is_jumping_ahead,
            is_duplicate_number,
            received_at: chrono::offset::Utc::now().to_rfc3339(),
        };

        csv_writer.serialize(supply_delta_log).unwrap();
        csv_writer.flush().unwrap();

        tracing::debug!("wrote supply delta log {}", supply_delta.block_number);
    }
}
