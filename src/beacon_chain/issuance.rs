use sqlx::PgPool;

use crate::{
    eth_units::GweiAmount,
    supply_projection::{GweiInTime, GweiInTimeRow},
};

use super::{
    beacon_time::{self, FirstOfDaySlot},
    deposits,
};

pub async fn store_issuance_for_day(
    pool: &PgPool,
    state_root: &str,
    FirstOfDaySlot(slot): FirstOfDaySlot,
    gwei: GweiAmount,
) {
    let gwei: i64 = gwei.to_owned().into();

    sqlx::query!(
        "
            INSERT INTO beacon_issuance (timestamp, state_root, gwei) VALUES ($1, $2, $3)
        ",
        beacon_time::get_timestamp(&slot),
        state_root,
        gwei,
    )
    .execute(pool)
    .await
    .unwrap();
}

pub fn calc_issuance(
    validator_balances_sum_gwei: &GweiAmount,
    deposit_sum_aggregated: &GweiAmount,
) -> GweiAmount {
    (*validator_balances_sum_gwei - *deposit_sum_aggregated) - deposits::INITIAL_DEPOSITS
}

pub async fn get_issuance_by_day(pool: &PgPool) -> sqlx::Result<Vec<GweiInTime>> {
    sqlx::query_as!(
        GweiInTimeRow,
        "
            SELECT timestamp, gwei FROM beacon_issuance
        "
    )
    .fetch_all(pool)
    .await
    .map(|rows| rows.iter().map(|row| row.into()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_issuance() {
        let validator_balances_sum_gwei = deposits::INITIAL_DEPOSITS + GweiAmount(100);
        let deposit_sum_aggregated = GweiAmount(50);

        assert_eq!(
            calc_issuance(&validator_balances_sum_gwei, &deposit_sum_aggregated),
            GweiAmount(50)
        )
    }
}
