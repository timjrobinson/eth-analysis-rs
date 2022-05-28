use sqlx::PgPool;

use super::slot_time;
use super::{gwei_amounts::GweiAmount, node::ValidatorBalance, slot_time::FirstOfDaySlot};

pub fn sum_validator_balances(validator_balances: Vec<ValidatorBalance>) -> GweiAmount {
    validator_balances
        .iter()
        .fold(GweiAmount(0), |sum, validator_balance| {
            sum + validator_balance.balance
        })
}

pub async fn store_validator_sum_for_day(
    pool: &PgPool,
    state_root: &str,
    FirstOfDaySlot(slot): &FirstOfDaySlot,
    gwei: &GweiAmount,
) {
    let gwei: i64 = gwei.to_owned().into();

    sqlx::query!(
        "
            INSERT INTO beacon_validators_balance (timestamp, state_root, gwei) VALUES ($1, $2, $3)
        ",
        slot_time::get_timestamp(slot),
        state_root,
        gwei,
    )
    .execute(pool)
    .await
    .unwrap();
}
