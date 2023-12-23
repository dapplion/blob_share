use std::collections::HashMap;
use std::hash::BuildHasher;
use std::{sync::Arc, time::Duration};

use actix_web::HttpRequest;
use actix_web::{get, web, HttpResponse};
use eyre::{bail, Result};
use log::{debug, error};
use prometheus::{register_gauge, Counter, Gauge, Histogram, TextEncoder};

use lazy_static::lazy_static;
use prometheus::{labels, register_counter, register_histogram};
use prometheus::{Encoder, ProtobufEncoder};
use reqwest::header::CONTENT_TYPE;
use reqwest::Method;
use url::Url;

use crate::utils::{e500, extract_bearer_token, is_ok_response, BasicAuthentication};
use crate::AppData;

lazy_static! {
    //
    // Block subscriber task
    //
    pub(crate) static ref BLOCK_SUBSCRIBER_TASK_TIMES: Histogram =
        register_histogram!("blobshare_block_subscriber_task_duration_seconds", "block subscriber task duration").unwrap();
    pub(crate) static ref BLOCK_SUBSCRIBER_TASK_ERRORS: Counter =
        register_counter!("blobshare_block_subscriber_task_errors", "block subscriber task errors").unwrap();
    pub(crate) static ref SYNC_BLOCK_KNOWN: Counter = register_counter!(
        "blobshare_sync_block_known",
        "Total count of attempts to import a known block"
    )
    .unwrap();
    pub(crate) static ref SYNC_REORGS: Counter = register_counter!(
        "blobshare_sync_reorgs_total",
        "Total count of re-org events, without tracking depth."
    )
    .unwrap();
    pub(crate) static ref SYNC_REORG_DEPTHS: Histogram = register_histogram!(
        "blobshare_sync_reorg_depth",
        "Depth of re-org events",
        vec![1., 2., 3., 4., 6., 8., 12., 16., 24., 32., 64., 128.]
    )
    .unwrap();
    pub(crate) static ref SYNC_BLOCK_WITH_BLOB_TXS: Counter = register_counter!(
        "blobshare_sync_blocks_with_blob_txs_total",
        "Total count of imported blocks with at least one blob tx"
    )
    .unwrap();
    pub(crate) static ref SYNC_BLOB_TXS_SYNCED: Counter =
        register_counter!("blobshare_sync_blob_txs_synced_total", "sync blob txs synced total").unwrap();
    pub(crate) static ref SYNC_HEAD_NUMBER: Gauge =
        register_gauge!("blobshare_sync_head_number", "sync head number").unwrap();
    pub(crate) static ref SYNC_ANCHOR_NUMBER: Gauge =
        register_gauge!("blobshare_sync_anchor_number", "sync anchor number").unwrap();
    pub(crate) static ref UNDERPRICED_TXS_EVICTED: Counter =
        register_counter!("blobshare_underpriced_txs_evicted_total", "underpriced txs evicted total").unwrap();
    pub(crate) static ref FINALIZED_TXS: Counter =
        register_counter!("blobshare_finalized_txs_total", "finalized txs total").unwrap();
    //
    // Blob sender task
    //
    pub(crate) static ref BLOB_SENDER_TASK_TIMES: Histogram =
        register_histogram!("blobshare_blob_sender_task_duration_seconds", "blob sender task duration seconds").unwrap();
    pub(crate) static ref BLOB_SENDER_TASK_ERRORS: Counter =
        register_counter!("blobshare_blob_sender_task_errors", "blob sender task errors").unwrap();
    pub(crate) static ref PACKING_TIMES: Histogram =
        register_histogram!("blobshare_packing_seconds", "packing seconds").unwrap();
    pub(crate) static ref PACKED_BLOB_ITEMS: Histogram = register_histogram!(
        "blob_share_packed_blob_items", "packed blob items",
        vec![1.,2.,3.,4.,6.,8.,12.,16.,24.,32.]
    ).unwrap();
    pub(crate) static ref PACKED_BLOB_USED_LEN: Histogram = register_histogram!(
        "blob_share_packed_blob_used_len", "packed blob used len",
        //   1/8     1/4     1/2     0.6     0.7     0.8      0.9      1
        vec![16384., 32768., 65536., 78643., 91750., 104857., 117964., 131072.]
    ).unwrap();
    //
    // Metrics
    //
    static ref PUSH_REQ_HISTOGRAM: Histogram = register_histogram!(
        "blobshare_push_request_duration_seconds",
        "The push request latencies in seconds."
    )
    .unwrap();

}

#[get("/metrics")]
pub(crate) async fn get_metrics(
    req: HttpRequest,
    data: web::Data<Arc<AppData>>,
) -> Result<HttpResponse, actix_web::Error> {
    debug!("{:?} {:?}", req.uri(), req.headers());

    // > Authorization: Bearer 12345678:AABBAABABABA==
    if let Some(expected_bearer_token) = &data.config.metrics_server_bearer_token {
        let bearer_token =
            extract_bearer_token(&req).map_err(actix_web::error::ErrorUnauthorized)?;
        if &bearer_token != expected_bearer_token {
            return Ok(HttpResponse::Unauthorized().finish());
        }
    }

    let (buf, content_type) = encode_metrics_plain_text(&prometheus::gather()).map_err(e500)?;

    Ok(HttpResponse::Ok().content_type(content_type).body(buf))
}

#[derive(Clone)]
pub struct PushMetricsConfig {
    pub url: Url,
    pub basic_auth: Option<BasicAuthentication>,
    pub interval: Duration,
    pub format: PushMetricsFormat,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum PushMetricsFormat {
    Protobuf,
    // TODO: Support InfluxLine for Grafana Cloud push
    // InfluxLine,
    PlainText,
}

pub async fn push_metrics_task(config: Option<PushMetricsConfig>) -> Result<()> {
    let config = if let Some(config) = config {
        Arc::new(config)
    } else {
        return Ok(());
    };

    let interval = config.interval;

    loop {
        let config = config.clone();
        match push_metrics(&config).await {
            Ok(s) => debug!("pushed metrics to gateway {}", s),
            Err(e) => error!("error pushing metrics to gateway {e:?}"),
        }

        // Wait some time before pushing metrics again, race against an interrupt signal to not
        // hang to process forever.
        tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(()),
            _ = tokio::time::sleep(interval) => {}
        }
    }
}

pub async fn push_metrics(config: &PushMetricsConfig) -> Result<String> {
    let mfs = prometheus::gather();
    let _timer = PUSH_REQ_HISTOGRAM.start_timer(); // drop as observe

    let client = reqwest::ClientBuilder::new().build().unwrap();

    let req = client.request(Method::POST, config.url.clone());

    let req = if let Some(auth) = &config.basic_auth {
        req.basic_auth(&auth.username, Some(&auth.password))
    } else {
        req
    };

    let grouping = labels! {};
    let (buf, format_type) = match config.format {
        PushMetricsFormat::Protobuf => encode_metrics_protobuf(grouping, mfs)?,
        PushMetricsFormat::PlainText => encode_metrics_plain_text(&mfs)?,
    };

    let response = req
        .header(CONTENT_TYPE, format_type)
        .body(buf)
        .send()
        .await?;

    Ok(is_ok_response(response).await?.text().await?)
}

/// Snipped copied from 'prometheus' library. The published version 0.13.3 is not compatible with
/// Grafana Cloud. It appends unconditionally to the URL path causing 404. Also the format expected
/// by Grafana Cloud is snappy compressed as per specs. However it's not implemented yet in
/// 'prometheus'. This snippet may work with Prometheus' push gateway but it's not tested.
/// <https://github.com/tikv/rust-prometheus/blob/f49c724df0e123520554664436da68e555593af0/src/push.rs#L123-L149>
fn encode_metrics_protobuf<S: BuildHasher>(
    grouping: HashMap<String, String, S>,
    mfs: Vec<prometheus::proto::MetricFamily>,
) -> Result<(Vec<u8>, String)> {
    let encoder = ProtobufEncoder::new();
    let mut buf = Vec::new();

    for mf in mfs {
        // Check for pre-existing grouping labels:
        for m in mf.get_metric() {
            for lp in m.get_label() {
                if lp.get_name() == "job" {
                    bail!(
                        "pushed metric {} already contains a \
                         job label",
                        mf.get_name()
                    );
                }
                if grouping.contains_key(lp.get_name()) {
                    bail!(
                        "pushed metric {} already contains \
                         grouping label {}",
                        mf.get_name(),
                        lp.get_name()
                    );
                }
            }
        }
        // Ignore error, `no metrics` and `no name`.
        let _ = encoder.encode(&[mf], &mut buf);
    }

    Ok((buf, encoder.format_type().to_string()))
}

fn encode_metrics_plain_text(mfs: &[prometheus::proto::MetricFamily]) -> Result<(Vec<u8>, String)> {
    let mut buffer = vec![];
    let encoder = TextEncoder::new();
    encoder.encode(mfs, &mut buffer).unwrap();
    Ok((buffer, encoder.format_type().to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use prometheus::{Counter, Registry};

    use super::*;

    #[test]
    fn test_encode_plain_text() {
        let registry = get_sample_metrics();
        let (buf, _) = encode_metrics_plain_text(&registry.gather()).unwrap();

        assert_eq!(
            std::str::from_utf8(&buf).unwrap(),
            "# HELP myprefix_test_counter test counter help
# TYPE myprefix_test_counter counter
myprefix_test_counter{mykey=\"myvalue\"} 0
"
        );
    }

    fn get_sample_metrics() -> Registry {
        let mut labels = HashMap::new();
        labels.insert("mykey".to_string(), "myvalue".to_string());
        let registry = Registry::new_custom(Some("myprefix".to_string()), Some(labels)).unwrap();
        let counter = Counter::new("test_counter", "test counter help").unwrap();
        registry.register(Box::new(counter.clone())).unwrap();
        registry
    }

    #[test]
    fn gather() {
        // By registering any value into all metrics this test checks that names are unique and
        // help sections valid

        BLOCK_SUBSCRIBER_TASK_TIMES.observe(0.);
        BLOCK_SUBSCRIBER_TASK_ERRORS.inc();
        SYNC_BLOCK_KNOWN.inc();
        SYNC_REORGS.inc();
        SYNC_REORG_DEPTHS.observe(0.);
        SYNC_BLOCK_WITH_BLOB_TXS.inc();
        SYNC_BLOB_TXS_SYNCED.inc();
        SYNC_HEAD_NUMBER.inc();
        SYNC_ANCHOR_NUMBER.inc();
        UNDERPRICED_TXS_EVICTED.inc();
        FINALIZED_TXS.inc();
        BLOB_SENDER_TASK_TIMES.observe(0.);
        BLOB_SENDER_TASK_ERRORS.inc();
        PACKING_TIMES.observe(0.);
        PACKED_BLOB_ITEMS.observe(0.);
        PACKED_BLOB_USED_LEN.observe(0.);
        PUSH_REQ_HISTOGRAM.observe(0.);

        encode_metrics_plain_text(&prometheus::gather()).unwrap();
    }
}
