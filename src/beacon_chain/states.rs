use sqlx::{PgExecutor, PgPool};
use thiserror::Error;

pub struct BeaconState {
    pub state_root: String,
    pub slot: i32,
    pub block_root: Option<String>,
}

#[derive(Error, Debug)]
pub enum GetLastStateError {
    #[error("failed to get last state from empty table")]
    EmptyTable,
    #[error(transparent)]
    SlqxError(#[from] sqlx::Error),
}

pub async fn get_last_state(pool: &PgPool) -> Result<BeaconState, GetLastStateError> {
    let row = sqlx::query_as!(
        BeaconState,
        r#"
            SELECT
                beacon_states.state_root,
                beacon_states.slot,
                beacon_blocks.block_root AS "block_root?"
            FROM beacon_states
            LEFT JOIN beacon_blocks ON beacon_blocks.state_root = beacon_states.state_root
            ORDER BY slot DESC
            LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;

    match row {
        None => Err(GetLastStateError::EmptyTable),
        Some(row) => Ok(row),
    }
}

pub async fn store_state<'a, A>(executor: A, state_root: &str, slot: &u32) -> sqlx::Result<()>
where
    A: PgExecutor<'a>,
{
    sqlx::query!(
        "
            INSERT INTO beacon_states (state_root, slot) VALUES ($1, $2)
        ",
        state_root,
        *slot as i32
    )
    .execute(executor)
    .await?;

    Ok(())
}
