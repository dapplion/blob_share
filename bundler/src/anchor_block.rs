use eyre::{eyre, Result};
use sqlx::MySqlPool;

use crate::{eth_provider::EthProvider, gas::block_gas_summary_from_block, sync::AnchorBlock};

pub(crate) async fn get_anchor_block(
    db_pool: &MySqlPool,
    provider: &EthProvider,
    starting_block: u64,
) -> Result<AnchorBlock> {
    // Second, fetch from DB
    if let Some(anchor_block) = fetch_anchor_block_from_db(db_pool).await? {
        if anchor_block.number >= starting_block {
            return Ok(anchor_block);
        }
        // else bootstrap from starting block
    }

    // Last initialize from network at starting point
    anchor_block_from_starting_block(provider, starting_block).await
}

/// Fetch AnchorBlock from DB
pub async fn fetch_anchor_block_from_db(db_pool: &MySqlPool) -> Result<Option<AnchorBlock>> {
    let row = sqlx::query!(
        "SELECT anchor_block_json FROM anchor_block ORDER BY block_number DESC LIMIT 1",
    )
    .fetch_optional(db_pool)
    .await?;

    Ok(match row {
        Some(row) => Some(serde_json::from_str(&row.anchor_block_json)?),
        None => None,
    })
}

/// Persist AnchorBlock to DB row.
/// TODO: Keep a single row with latest block
pub async fn persist_anchor_block_to_db(
    db_pool: &MySqlPool,
    anchor_block: AnchorBlock,
) -> Result<()> {
    // Serialize the AnchorBlock (except the block_number field) to a JSON string
    let anchor_block_json = serde_json::to_string(&anchor_block)?;
    let block_number = anchor_block.number;

    // Insert the data into the database
    sqlx::query!(
        "INSERT INTO anchor_block (anchor_block_json, block_number) VALUES (?, ?)",
        anchor_block_json,
        block_number
    )
    .execute(db_pool)
    .await?;

    Ok(())
}

/// Initialize empty anchor block state from a network block
pub async fn anchor_block_from_starting_block(
    provider: &EthProvider,
    starting_block: u64,
) -> Result<AnchorBlock> {
    let anchor_block = provider
        .get_block(starting_block)
        .await?
        .ok_or_else(|| eyre!("genesis block not available"))?;
    let hash = anchor_block
        .hash
        .ok_or_else(|| eyre!("block has no hash property"))?;
    let number = anchor_block
        .number
        .ok_or_else(|| eyre!("block has no number property"))?
        .as_u64();
    Ok(AnchorBlock {
        hash,
        number,
        gas: block_gas_summary_from_block(&anchor_block)?,
        // At genesis all balances are zero
        finalized_balances: <_>::default(),
    })
}
