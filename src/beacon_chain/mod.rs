pub mod balances;
mod blocks;
mod deposits;
pub mod effective_balance_sums;
mod issuance;
mod node;
mod rewards;
pub mod states;
mod sync;
mod units;
mod withdrawals;

pub use balances::backfill;
pub use balances::get_balances_by_state_root;
pub use balances::get_validator_balances_by_start_of_day;
pub use balances::store_validators_balance;
pub use balances::sum_validator_balances;
pub use balances::BeaconBalancesSum;

pub use blocks::get_block_before_slot;
pub use blocks::get_block_by_slot;
pub use blocks::heal_block_hashes;
pub use blocks::store_block;
pub use blocks::GENESIS_PARENT_ROOT;

use chrono::DateTime;
use chrono::Utc;
pub use deposits::get_deposits_sum_by_state_root;
pub use deposits::BeaconDepositsSum;

pub use issuance::update_issuance_estimate;
pub use issuance::IssuanceStore;
pub use issuance::IssuanceStorePostgres;

#[cfg(test)]
pub use node::tests::BeaconBlockBuilder;
#[cfg(test)]
pub use node::tests::BeaconHeaderSignedEnvelopeBuilder;
pub use node::BeaconHeader;
pub use node::BeaconHeaderEnvelope;
pub use node::BeaconHeaderSignedEnvelope;
pub use node::BeaconNode;
pub use node::BeaconNodeHttp;
pub use node::BlockId;
pub use node::StateRoot;

pub use rewards::update_validator_rewards;

pub use states::get_last_state;
pub use states::get_state_root_by_slot;
pub use states::heal_beacon_states;
pub use states::store_state;

pub use sync::sync_beacon_states;

pub use units::slot_from_string;
pub use units::Slot;

use lazy_static::lazy_static;

use crate::env;

pub const FIRST_POST_MERGE_SLOT: Slot = Slot(4700013);
pub const FIRST_POST_LONDON_SLOT: Slot = Slot(1778566);

lazy_static! {
    static ref BEACON_URL: String = env::get_env_var_unsafe("BEACON_URL");
    pub static ref GENESIS_TIMESTAMP: DateTime<Utc> = "2020-12-01T12:00:23Z".parse().unwrap();
    pub static ref SHAPELLA_SLOT: Slot = Slot(6209536);
}

#[cfg(test)]
pub mod tests {
    use sqlx::{Acquire, PgConnection};

    use crate::units::GweiNewtype;

    use super::{
        node::{tests::BeaconBlockBuilder, BeaconBlock},
        *,
    };

    pub async fn store_test_block(executor: &mut PgConnection, test_id: &str) {
        let header = BeaconHeaderSignedEnvelopeBuilder::new(test_id).build();
        let block = Into::<BeaconBlockBuilder>::into(&header).build();

        store_custom_test_block(executor, &header, &block).await;
    }

    pub async fn store_custom_test_block(
        executor: &mut PgConnection,
        header: &BeaconHeaderSignedEnvelope,
        block: &BeaconBlock,
    ) {
        store_state(
            executor.acquire().await.unwrap(),
            &header.header.message.state_root,
            &header.header.message.slot,
        )
        .await;

        store_block(
            executor,
            block,
            &GweiNewtype(0),
            &GweiNewtype(0),
            &GweiNewtype(0),
            &GweiNewtype(0),
            header,
        )
        .await;
    }
}
