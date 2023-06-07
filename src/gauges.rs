/// Calculates the various rates displayed in the gauge charts.
///
/// For since burn issuance we consider beacon chain issuance only giving an idea of the rate of
/// issuance on the beacon chain based on data since the burn, not the execution chain, which has
/// issued many ETH since then too. To get a feel for this scenario we offer the "simulate pow"
/// toggle which sets the rate to an estimate of only the pow issuance.
use std::collections::HashMap;

use anyhow::Result;
use chrono::{DateTime, Utc};
use enum_iterator::all;
use futures::join;
use serde::Serialize;
use sqlx::PgPool;

use crate::{
    beacon_chain::IssuanceStore,
    burn_sums::{BurnSums, EthUsdAmount},
    caching::{self, CacheKey},
    execution_chain::{BlockNumber, ExecutionNodeBlock},
    performance::TimedExt,
    time_frames::TimeFrame,
    units::{EthNewtype, UsdNewtype},
    usd_price::EthPriceStore,
};

const PROOF_OF_WORK_DAILY_ISSUANCE_ESTIMATE: f64 = 13500.0;
const DAYS_PER_YEAR: f64 = 365.25;
const PROOF_OF_WORK_YEARLY_ISSUANCE_ESTIMATE: f64 =
    PROOF_OF_WORK_DAILY_ISSUANCE_ESTIMATE * DAYS_PER_YEAR;
const HOURS_PER_DAY: f64 = 24.0;
const MINUTES_PER_HOUR: f64 = 60.0;
const MINUTES_PER_YEAR: f64 = MINUTES_PER_HOUR * HOURS_PER_DAY * DAYS_PER_YEAR;

#[derive(Debug, Serialize)]
pub struct GaugeRatesTimeFrame {
    block_number: BlockNumber,
    burn_rate_yearly: EthUsdAmount,
    issuance_rate_yearly: EthUsdAmount,
    issuance_rate_yearly_pow: EthUsdAmount,
    supply_growth_rate_yearly: f64,
    supply_growth_rate_yearly_pow: f64,
    timestamp: DateTime<Utc>,
}

pub type GaugeRates = HashMap<TimeFrame, GaugeRatesTimeFrame>;

pub async fn on_new_block(
    db_pool: &PgPool,
    eth_price_store: &impl EthPriceStore,
    issuance_store: &impl IssuanceStore,
    block: &ExecutionNodeBlock,
    burn_sums: &BurnSums,
    eth_supply: &EthNewtype,
) -> Result<()> {
    let mut gauge_rates: GaugeRates = HashMap::new();

    for time_frame in all::<TimeFrame>() {
        let burn_rate_yearly = burn_sums
            .get(&time_frame)
            .unwrap()
            .sum
            .yearly_rate_from_time_frame(time_frame);

        let (issuance_time_frame, usd_price_average) = join!(
            issuance_store
                .issuance_from_time_frame(block, &time_frame)
                .timed(&format!("issuance_from_time_frame_{time_frame}")),
            eth_price_store
                .average_from_time_range(time_frame.start_timestamp(block), block.timestamp,)
                .timed(&format!("usd_price::average_from_time_range_{time_frame}"))
        );

        let issuance_time_frame_eth: EthNewtype = issuance_time_frame?.into();
        let year_time_frame_fraction =
            MINUTES_PER_YEAR / time_frame.duration().num_minutes() as f64;
        let issuance_rate_yearly_eth =
            EthNewtype(issuance_time_frame_eth.0 * year_time_frame_fraction);
        let issuance_rate_yearly = EthUsdAmount {
            eth: issuance_rate_yearly_eth,
            // It'd be nice to have a precise estimate of the USD issuance, but we don't have usd prices per
            // slot yet. We use an average usd price over the time frame instead.
            usd: UsdNewtype(issuance_rate_yearly_eth.0 * usd_price_average.0),
        };

        let issuance_rate_yearly_pow = EthUsdAmount {
            eth: EthNewtype(PROOF_OF_WORK_YEARLY_ISSUANCE_ESTIMATE),
            usd: UsdNewtype(PROOF_OF_WORK_YEARLY_ISSUANCE_ESTIMATE * usd_price_average.0),
        };

        let supply_growth_rate_yearly = {
            let eth_burn = burn_rate_yearly.eth;
            let eth_issuance = issuance_rate_yearly.eth;
            let yearly_delta = eth_issuance - eth_burn;
            yearly_delta.0 / eth_supply.0
        };

        let supply_growth_rate_yearly_pow = {
            let eth_burn = burn_rate_yearly.eth;
            let eth_issuance = issuance_rate_yearly_pow.eth;
            let yearly_delta = eth_issuance - eth_burn;
            yearly_delta.0 / eth_supply.0
        };

        gauge_rates.insert(
            time_frame,
            GaugeRatesTimeFrame {
                block_number: block.number,
                burn_rate_yearly,
                issuance_rate_yearly,
                issuance_rate_yearly_pow,
                supply_growth_rate_yearly,
                supply_growth_rate_yearly_pow,
                timestamp: block.timestamp,
            },
        );
    }

    caching::update_and_publish(db_pool, &CacheKey::GaugeRates, gauge_rates).await;

    Ok(())
}
