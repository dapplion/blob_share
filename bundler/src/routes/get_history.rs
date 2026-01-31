use actix_web::{get, web, HttpResponse};
use ethers::types::Address;
use serde::Deserialize;
use std::sync::Arc;

use crate::utils::e500;
use crate::AppData;

/// Default number of history entries per page.
const DEFAULT_LIMIT: u32 = 50;
/// Maximum allowed limit to prevent excessive queries.
const MAX_LIMIT: u32 = 200;

#[derive(Debug, Deserialize)]
pub(crate) struct HistoryQuery {
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[tracing::instrument(skip(data))]
#[get("/v1/history/{address}")]
pub(crate) async fn get_history(
    data: web::Data<Arc<AppData>>,
    address: web::Path<Address>,
    query: web::Query<HistoryQuery>,
) -> Result<HttpResponse, actix_web::Error> {
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
    let offset = query.offset.unwrap_or(0);

    let response = data
        .history_for_address(&address, limit, offset)
        .await
        .map_err(e500)?;

    Ok(HttpResponse::Ok().json(response))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_query_deserialize_empty() {
        let q: HistoryQuery = serde_json::from_str("{}").unwrap();
        assert!(q.limit.is_none());
        assert!(q.offset.is_none());
    }

    #[test]
    fn history_query_deserialize_with_values() {
        let q: HistoryQuery = serde_json::from_str(r#"{"limit": 10, "offset": 20}"#).unwrap();
        assert_eq!(q.limit, Some(10));
        assert_eq!(q.offset, Some(20));
    }

    #[test]
    fn limit_clamped_to_max() {
        let requested = 500u32;
        let clamped = requested.min(MAX_LIMIT);
        assert_eq!(clamped, MAX_LIMIT);
    }

    #[test]
    fn limit_within_max_unchanged() {
        let requested = 100u32;
        let clamped = requested.min(MAX_LIMIT);
        assert_eq!(clamped, 100);
    }

    #[test]
    fn default_limit_applied_when_none() {
        let q = HistoryQuery {
            limit: None,
            offset: None,
        };
        let effective_limit = q.limit.unwrap_or(DEFAULT_LIMIT).min(MAX_LIMIT);
        assert_eq!(effective_limit, DEFAULT_LIMIT);
    }
}
