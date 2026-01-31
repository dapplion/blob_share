use std::collections::HashMap;
use std::hash::BuildHasher;
use std::{sync::Arc, time::Duration};

use actix_web::HttpRequest;
use actix_web::{get, web, HttpResponse};
use eyre::{bail, Result};
use log::{debug, error};
use prometheus::{
    register_gauge, register_histogram_vec, register_int_counter_vec, Counter, Gauge, Histogram,
    HistogramVec, IntCounterVec, TextEncoder,
};

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
    pub(crate) static ref BLOCK_SUBSCRIBER_TASK_RETRIES: Counter =
        register_counter!("blobshare_block_subscriber_task_retries", "block subscriber task retries after transient errors").unwrap();
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
    pub(crate) static ref FINALIZED_TXS: Counter =
        register_counter!("blobshare_finalized_txs_total", "finalized txs total").unwrap();
    //
    // Blob sender task
    //
    pub(crate) static ref BLOB_SENDER_TASK_TIMES: Histogram =
        register_histogram!("blobshare_blob_sender_task_duration_seconds", "blob sender task duration seconds").unwrap();
    pub(crate) static ref BLOB_SENDER_TASK_ERRORS: Counter =
        register_counter!("blobshare_blob_sender_task_errors", "blob sender task errors").unwrap();
    pub(crate) static ref BLOB_SENDER_TASK_RETRIES: Counter =
        register_counter!("blobshare_blob_sender_task_retries", "blob sender task retries after transient errors").unwrap();
    pub(crate) static ref PACKING_TIMES: Histogram =
        register_histogram!("blobshare_packing_seconds", "packing seconds").unwrap();
    pub(crate) static ref PACKED_BLOB_ITEMS: Histogram = register_histogram!(
        "blobshare_packed_blob_items", "packed blob items",
        vec![1.,2.,3.,4.,6.,8.,12.,16.,24.,32.]
    ).unwrap();
    pub(crate) static ref PACKED_BLOB_USED_LEN: Histogram = register_histogram!(
        "blobshare_packed_blob_used_len", "packed blob used len",
        //   1/8     1/4     1/2     0.6     0.7     0.8      0.9      1
        vec![16384., 32768., 65536., 78643., 91750., 104857., 117964., 131072.]
    ).unwrap();
    //
    // Remote node tracker task
    //
    pub(crate) static ref REMOTE_NODE_HEAD_BLOCK_NUMBER: Gauge =
        register_gauge!("blobshare_remote_node_head_block_number", "remote node head block number").unwrap();
    pub(crate) static ref REMOTE_NODE_HEAD_BLOCK_FETCH_ERRORS: Counter =
        register_counter!("blobshare_remote_node_head_block_fetch_errors", "remote node head block fetch errors").unwrap();
    pub(crate) static ref SENDER_BALANCE_REMOTE_HEAD: Gauge =
        register_gauge!("blobshare_sender_balance_remote_head", "sender balance at remote head").unwrap();
    pub(crate) static ref SENDER_NONCE_REMOTE_HEAD: Gauge =
        register_gauge!("blobshare_sender_nonce_remote_head", "sender nonce at remote head").unwrap();
    //
    // Data intent
    //
    pub(crate) static ref PENDING_INTENTS_CACHE: Gauge =
        register_gauge!("blobshare_pending_intents_cache", "pending intents cache").unwrap();
    pub(crate) static ref INCLUDED_INTENTS_CACHE: Gauge =
        register_gauge!("blobshare_included_intents_cache", "included intents cache").unwrap();
    pub(crate) static ref EVICTED_STALE_INTENTS: Counter =
        register_counter!("blobshare_evicted_stale_intents_total", "total stale underpriced intents evicted").unwrap();
    pub(crate) static ref NONCE_DEADLOCK_SELF_TRANSFERS: Counter =
        register_counter!("blobshare_nonce_deadlock_self_transfers_total", "total self-transfer transactions sent to resolve nonce deadlocks").unwrap();
    //
    // API endpoint metrics
    //
    pub(crate) static ref API_REQUESTS_TOTAL: IntCounterVec = register_int_counter_vec!(
        "blobshare_api_requests_total",
        "Total API requests by method, path, and status code",
        &["method", "path", "status"]
    )
    .unwrap();
    pub(crate) static ref API_REQUEST_DURATION_SECONDS: HistogramVec = register_histogram_vec!(
        "blobshare_api_request_duration_seconds",
        "API request duration in seconds by method and path",
        &["method", "path"]
    )
    .unwrap();
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
    // > Authorization: Bearer 12345678:AABBAABABABA==
    if let Some(expected_bearer_token) = &data.config.metrics_server_bearer_token {
        let bearer_token =
            extract_bearer_token(&req).map_err(actix_web::error::ErrorUnauthorized)?;
        if &bearer_token != expected_bearer_token {
            return Ok(HttpResponse::Unauthorized().finish());
        }
    }

    // Trigger collect on passive metrics
    data.collect_metrics().await;
    let mfs = prometheus::gather();

    let (buf, content_type) = encode_metrics_plain_text(&mfs).map_err(e500)?;

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
    InfluxLine,
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

    let client = reqwest::ClientBuilder::new().build()?;

    let req = client.request(Method::POST, config.url.clone());

    let req = if let Some(auth) = &config.basic_auth {
        req.basic_auth(&auth.username, Some(&auth.password))
    } else {
        req
    };

    let grouping = labels! {};
    let (buf, format_type) = match config.format {
        PushMetricsFormat::Protobuf => encode_metrics_protobuf(grouping, mfs)?,
        PushMetricsFormat::InfluxLine => encode_metrics_influx_line(&mfs),
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
    encoder.encode(mfs, &mut buffer)?;
    Ok((buffer, encoder.format_type().to_string()))
}

/// Encode metrics in InfluxDB line protocol format for Grafana Cloud push.
///
/// Format: `measurement,tag1=val1 field1=value1`
///
/// Each Prometheus metric becomes one or more lines depending on its type:
/// - Counter/Gauge/Untyped: single line with `value=<val>`
/// - Histogram: one line per bucket (`le` tag, `bucket=<count>`), plus `_sum` and `_count` lines
/// - Summary: one line per quantile (`quantile` tag, `value=<val>`), plus `_sum` and `_count` lines
fn encode_metrics_influx_line(mfs: &[prometheus::proto::MetricFamily]) -> (Vec<u8>, String) {
    use prometheus::proto::MetricType;
    use std::fmt::Write;

    let mut out = String::new();

    for mf in mfs {
        let name = mf.get_name();
        let metric_type = mf.get_field_type();

        for m in mf.get_metric() {
            let tags = format_influx_tags(m.get_label());

            match metric_type {
                MetricType::COUNTER => {
                    let val = m.get_counter().get_value();
                    write_influx_line(&mut out, name, &tags, "value", val);
                }
                MetricType::GAUGE => {
                    let val = m.get_gauge().get_value();
                    write_influx_line(&mut out, name, &tags, "value", val);
                }
                MetricType::HISTOGRAM => {
                    let h = m.get_histogram();
                    for b in h.get_bucket() {
                        let le = b.get_upper_bound();
                        let le_str = format_f64(le);
                        let bucket_tags = if tags.is_empty() {
                            format!("le={le_str}")
                        } else {
                            format!("{tags},le={le_str}")
                        };
                        let _ = writeln!(
                            out,
                            "{name}_bucket,{bucket_tags} value={}u",
                            b.get_cumulative_count()
                        );
                    }
                    // +Inf bucket
                    let inf_tags = if tags.is_empty() {
                        "le=+Inf".to_string()
                    } else {
                        format!("{tags},le=+Inf")
                    };
                    let _ = writeln!(
                        out,
                        "{name}_bucket,{inf_tags} value={}u",
                        h.get_sample_count()
                    );
                    let sum_name = format!("{name}_sum");
                    write_influx_line(&mut out, &sum_name, &tags, "value", h.get_sample_sum());
                    let count_name = format!("{name}_count");
                    write_influx_line_uint(
                        &mut out,
                        &count_name,
                        &tags,
                        "value",
                        h.get_sample_count(),
                    );
                }
                MetricType::SUMMARY => {
                    let s = m.get_summary();
                    for q in s.get_quantile() {
                        let quantile_str = format_f64(q.get_quantile());
                        let q_tags = if tags.is_empty() {
                            format!("quantile={quantile_str}")
                        } else {
                            format!("{tags},quantile={quantile_str}")
                        };
                        write_influx_line(&mut out, name, &q_tags, "value", q.get_value());
                    }
                    let sum_name = format!("{name}_sum");
                    write_influx_line(&mut out, &sum_name, &tags, "value", s.get_sample_sum());
                    let count_name = format!("{name}_count");
                    write_influx_line_uint(
                        &mut out,
                        &count_name,
                        &tags,
                        "value",
                        s.get_sample_count(),
                    );
                }
                MetricType::UNTYPED => {
                    let val = m.get_untyped().get_value();
                    write_influx_line(&mut out, name, &tags, "value", val);
                }
            }
        }
    }

    (out.into_bytes(), "text/plain".to_string())
}

/// Format an f64 for InfluxDB line protocol, using "+Inf" / "-Inf" / "NaN" for special values.
fn format_f64(v: f64) -> String {
    if v == f64::INFINITY {
        "+Inf".to_string()
    } else if v == f64::NEG_INFINITY {
        "-Inf".to_string()
    } else if v.is_nan() {
        "NaN".to_string()
    } else {
        v.to_string()
    }
}

/// Format Prometheus labels as InfluxDB tag set (comma-separated key=value pairs).
fn format_influx_tags(labels: &[prometheus::proto::LabelPair]) -> String {
    labels
        .iter()
        .map(|lp| format!("{}={}", lp.get_name(), lp.get_value()))
        .collect::<Vec<_>>()
        .join(",")
}

/// Write a single InfluxDB line protocol entry with a float value.
fn write_influx_line(out: &mut String, measurement: &str, tags: &str, field: &str, value: f64) {
    use std::fmt::Write;
    if tags.is_empty() {
        let _ = writeln!(out, "{measurement} {field}={value}");
    } else {
        let _ = writeln!(out, "{measurement},{tags} {field}={value}");
    }
}

/// Write a single InfluxDB line protocol entry with an unsigned integer value.
fn write_influx_line_uint(
    out: &mut String,
    measurement: &str,
    tags: &str,
    field: &str,
    value: u64,
) {
    use std::fmt::Write;
    if tags.is_empty() {
        let _ = writeln!(out, "{measurement} {field}={value}u");
    } else {
        let _ = writeln!(out, "{measurement},{tags} {field}={value}u");
    }
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
    fn test_encode_influx_line_counter() {
        let registry = get_sample_metrics();
        let (buf, content_type) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        assert_eq!(content_type, "text/plain");
        assert_eq!(output, "myprefix_test_counter,mykey=myvalue value=0\n");
    }

    #[test]
    fn test_encode_influx_line_counter_no_labels() {
        let registry = Registry::new_custom(Some("test".to_string()), None).unwrap();
        let counter = Counter::new("requests", "total requests").unwrap();
        registry.register(Box::new(counter.clone())).unwrap();
        counter.inc_by(42.0);

        let (buf, _) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        assert_eq!(output, "test_requests value=42\n");
    }

    #[test]
    fn test_encode_influx_line_gauge() {
        let registry = Registry::new_custom(Some("test".to_string()), None).unwrap();
        let gauge = prometheus::Gauge::new("temperature", "current temperature").unwrap();
        registry.register(Box::new(gauge.clone())).unwrap();
        gauge.set(36.6);

        let (buf, _) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        assert_eq!(output, "test_temperature value=36.6\n");
    }

    #[test]
    fn test_encode_influx_line_histogram() {
        let registry = Registry::new_custom(Some("test".to_string()), None).unwrap();
        let hist = prometheus::Histogram::with_opts(
            prometheus::HistogramOpts::new("duration", "request duration").buckets(vec![0.1, 0.5]),
        )
        .unwrap();
        registry.register(Box::new(hist.clone())).unwrap();
        hist.observe(0.05);
        hist.observe(0.3);

        let (buf, _) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        // Should have bucket lines for 0.1, 0.5, +Inf, plus _sum and _count
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "test_duration_bucket,le=0.1 value=1u");
        assert_eq!(lines[1], "test_duration_bucket,le=0.5 value=2u");
        assert_eq!(lines[2], "test_duration_bucket,le=+Inf value=2u");
        assert_eq!(lines[3], "test_duration_sum value=0.35");
        assert_eq!(lines[4], "test_duration_count value=2u");
    }

    #[test]
    fn test_encode_influx_line_labeled_histogram() {
        let registry = Registry::new_custom(Some("test".to_string()), None).unwrap();
        let hist_vec = prometheus::HistogramVec::new(
            prometheus::HistogramOpts::new("latency", "latency by method").buckets(vec![1.0]),
            &["method"],
        )
        .unwrap();
        registry.register(Box::new(hist_vec.clone())).unwrap();
        hist_vec.with_label_values(&["GET"]).observe(0.5);

        let (buf, _) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "test_latency_bucket,method=GET,le=1 value=1u");
        assert_eq!(lines[1], "test_latency_bucket,method=GET,le=+Inf value=1u");
        assert_eq!(lines[2], "test_latency_sum,method=GET value=0.5");
        assert_eq!(lines[3], "test_latency_count,method=GET value=1u");
    }

    #[test]
    fn test_encode_influx_line_multiple_metrics() {
        let registry = Registry::new_custom(Some("app".to_string()), None).unwrap();

        let counter = Counter::new("errors", "error count").unwrap();
        registry.register(Box::new(counter.clone())).unwrap();
        counter.inc_by(5.0);

        let gauge = prometheus::Gauge::new("uptime", "uptime seconds").unwrap();
        registry.register(Box::new(gauge.clone())).unwrap();
        gauge.set(1000.0);

        let (buf, _) = encode_metrics_influx_line(&registry.gather());
        let output = std::str::from_utf8(&buf).unwrap();

        assert!(output.contains("app_errors value=5\n"));
        assert!(output.contains("app_uptime value=1000\n"));
    }

    #[test]
    fn test_format_f64_special_values() {
        assert_eq!(format_f64(f64::INFINITY), "+Inf");
        assert_eq!(format_f64(f64::NEG_INFINITY), "-Inf");
        assert_eq!(format_f64(f64::NAN), "NaN");
        assert_eq!(format_f64(1.5), "1.5");
        assert_eq!(format_f64(0.0), "0");
    }

    #[test]
    fn gather() {
        // By registering any value into all metrics this test checks that names are unique and
        // help sections valid

        BLOCK_SUBSCRIBER_TASK_TIMES.observe(0.);
        BLOCK_SUBSCRIBER_TASK_ERRORS.inc();
        BLOCK_SUBSCRIBER_TASK_RETRIES.inc();
        SYNC_BLOCK_KNOWN.inc();
        SYNC_REORGS.inc();
        SYNC_REORG_DEPTHS.observe(0.);
        SYNC_BLOCK_WITH_BLOB_TXS.inc();
        SYNC_BLOB_TXS_SYNCED.inc();
        SYNC_HEAD_NUMBER.inc();
        SYNC_ANCHOR_NUMBER.inc();
        FINALIZED_TXS.inc();
        BLOB_SENDER_TASK_TIMES.observe(0.);
        BLOB_SENDER_TASK_ERRORS.inc();
        BLOB_SENDER_TASK_RETRIES.inc();
        PACKING_TIMES.observe(0.);
        PACKED_BLOB_ITEMS.observe(0.);
        PACKED_BLOB_USED_LEN.observe(0.);
        PUSH_REQ_HISTOGRAM.observe(0.);
        EVICTED_STALE_INTENTS.inc();
        NONCE_DEADLOCK_SELF_TRANSFERS.inc();
        API_REQUESTS_TOTAL
            .with_label_values(&["GET", "/v1/health", "200"])
            .inc();
        API_REQUEST_DURATION_SECONDS
            .with_label_values(&["GET", "/v1/health"])
            .observe(0.);

        encode_metrics_plain_text(&prometheus::gather()).unwrap();
    }
}
