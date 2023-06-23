use std::collections::HashMap;

use crate::{
    beacon_chain::{self, node::BeaconNodeHttp, sync, Slot},
    job_progress::JobProgress,
    key_value_store::KeyValueStorePostgres,
};
use pit_wall::Progress;
use sqlx::postgres::PgPoolOptions;
use tracing::{debug, info, warn};

use crate::{beacon_chain::BeaconNode, db, log};

// The first slot we have stored.
const FIRST_STORED_ETH_SUPPLY_SLOT: Slot = Slot(0);

const HEAL_BEACON_STATES_KEY: &str = "heal-beacon-states";

pub async fn heal_beacon_states() {
    log::init_with_env();

    info!("healing reorged states");

    let db_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&db::get_db_url_with_name("heal-beacon-states"))
        .await
        .unwrap();
    let key_value_store = KeyValueStorePostgres::new(db_pool.clone());
    let job_progress = JobProgress::new(HEAL_BEACON_STATES_KEY, &key_value_store);

    let beacon_node = BeaconNodeHttp::new();
    let last_slot = beacon_chain::get_last_state(&db_pool)
        .await
        .expect("a beacon state should be stored before trying to heal any")
        .slot
        .0;
    let last_checked = job_progress.get().await;
    let starting_slot = last_checked.unwrap_or(FIRST_STORED_ETH_SUPPLY_SLOT).0;

    debug!(
        %starting_slot,
        %last_slot,
        "checking first stored slot to last slot for gaps"
    );

    let work_todo: u64 = (last_slot - starting_slot) as u64;
    let mut progress = Progress::new("heal-beacon-states", work_todo);

    let slots = (starting_slot..=last_slot).collect::<Vec<i32>>();

    for chunk in slots.chunks(10000) {
        let first = chunk.first().unwrap();
        let last = chunk.last().unwrap();
        let stored_states = sqlx::query!(
            "
                SELECT
                    slot,
                    state_root
                FROM
                    beacon_states
                WHERE
                    slot >= $1
                AND
                    slot <= $2
                ORDER BY
                    slot ASC
            ",
            *first,
            *last
        )
        .fetch_all(&db_pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| (row.slot, row.state_root))
        .collect::<HashMap<i32, String>>();

        for slot in *first..=*last {
            let stored_state_root = stored_states.get(&slot).unwrap();
            let state_root = beacon_node
                .get_state_root_by_slot(&slot.into())
                .await
                .unwrap()
                .expect("expect state_root to exist for historic slots");

            if *stored_state_root != state_root {
                warn!("state root mismatch, rolling back stored and resyncing");
                sync::rollback_slot(&mut db_pool.acquire().await.unwrap(), &slot.into())
                    .await
                    .unwrap();
                sync::sync_slot_by_state_root(&db_pool, &beacon_node, &state_root, &slot.into())
                    .await
                    .unwrap();
                info!(%slot, "healed state at slot");
            }

            progress.inc_work_done();
        }

        job_progress.set(&last.into()).await;
        info!("{}", progress.get_progress_string());
    }

    info!("done healing beacon states");
}
