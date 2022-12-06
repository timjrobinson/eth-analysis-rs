use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Deserialize;

use crate::beacon_chain::{beacon_time, Slot};

use super::PhoenixRefresher;

#[derive(Debug, Deserialize)]
struct SupplyParts {
    pub slot: Slot,
}

impl SupplyParts {
    async fn get_current() -> reqwest::Result<SupplyParts> {
        reqwest::get("https://ultrasound.money/api/v2/fees/supply-parts")
            .await?
            .error_for_status()?
            .json::<SupplyParts>()
            .await
    }
}

pub struct SupplyPartsMonitor {}

impl SupplyPartsMonitor {
    pub fn new() -> Self {
        Self {}
    }

    pub async fn get_current_timestamp(&self) -> Result<DateTime<Utc>> {
        SupplyParts::get_current()
            .await
            .map(|supply_parts| beacon_time::date_time_from_slot(&supply_parts.slot))
            .map_err(|e| e.into())
    }
}

#[async_trait]
impl PhoenixRefresher for SupplyPartsMonitor {
    async fn refresh(&mut self) -> Result<DateTime<Utc>> {
        self.get_current_timestamp().await
    }
}
