use eth_analysis::{
    beacon_chain::{
        backfill::{backfill_balances, Granularity},
        Slot,
    },
    db, log,
};
use tracing::info;

#[tokio::main]
pub async fn main() {
    log::init_with_env();

    info!("backfilling hourly beacon balances");

    let db_pool = db::get_db_pool("backfill-hourly-balances").await;

    backfill_balances(&db_pool, &Granularity::Hour, &Slot(0)).await;

    info!("done backfilling hourly beacon balances");
}
