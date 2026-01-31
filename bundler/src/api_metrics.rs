use std::future::{ready, Future, Ready};
use std::pin::Pin;
use std::time::Instant;

use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::Error;

use crate::metrics::{API_REQUESTS_TOTAL, API_REQUEST_DURATION_SECONDS};

/// Middleware that records per-endpoint request count and duration metrics.
pub struct ApiMetrics;

impl<S, B> Transform<S, ServiceRequest> for ApiMetrics
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = ApiMetricsMiddleware<S>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(ApiMetricsMiddleware { service }))
    }
}

pub struct ApiMetricsMiddleware<S> {
    service: S,
}

impl<S, B> Service<ServiceRequest> for ApiMetricsMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>>>>;

    fn poll_ready(
        &self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let method = req.method().to_string();
        // Use the matched route pattern (e.g. "/v1/data/{id}") to avoid high-cardinality labels.
        // Falls back to the raw path if no pattern matched.
        let path = req
            .match_pattern()
            .unwrap_or_else(|| req.path().to_string());

        let start = Instant::now();
        let fut = self.service.call(req);

        Box::pin(async move {
            let res = fut.await?;
            let elapsed = start.elapsed().as_secs_f64();
            let status = res.status().as_u16().to_string();

            API_REQUESTS_TOTAL
                .with_label_values(&[&method, &path, &status])
                .inc();
            API_REQUEST_DURATION_SECONDS
                .with_label_values(&[&method, &path])
                .observe(elapsed);

            Ok(res)
        })
    }
}

#[cfg(test)]
mod tests {
    use actix_web::{test, web, App, HttpResponse};

    use super::*;

    #[actix_web::test]
    async fn test_api_metrics_middleware_records_metrics() {
        let app = test::init_service(
            App::new()
                .wrap(ApiMetrics)
                .route("/v1/test", web::get().to(HttpResponse::Ok)),
        )
        .await;

        let req = test::TestRequest::get().uri("/v1/test").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let count = API_REQUESTS_TOTAL
            .with_label_values(&["GET", "/v1/test", "200"])
            .get();
        assert!(count >= 1, "expected counter >= 1, got {count}");

        let metric = API_REQUEST_DURATION_SECONDS.with_label_values(&["GET", "/v1/test"]);
        assert!(
            metric.get_sample_count() >= 1,
            "expected at least 1 observation"
        );
    }

    #[actix_web::test]
    async fn test_api_metrics_uses_route_pattern() {
        let app = test::init_service(
            App::new()
                .wrap(ApiMetrics)
                .route("/v1/data/{id}", web::get().to(HttpResponse::Ok)),
        )
        .await;

        // Request with a specific ID - the label should use the pattern, not the actual value
        let req = test::TestRequest::get()
            .uri("/v1/data/some-uuid-123")
            .to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 200);

        let count = API_REQUESTS_TOTAL
            .with_label_values(&["GET", "/v1/data/{id}", "200"])
            .get();
        assert!(count >= 1, "expected counter >= 1, got {count}");
    }

    #[actix_web::test]
    async fn test_api_metrics_records_error_status() {
        let app = test::init_service(
            App::new()
                .wrap(ApiMetrics)
                .route("/v1/fail", web::get().to(HttpResponse::InternalServerError)),
        )
        .await;

        let req = test::TestRequest::get().uri("/v1/fail").to_request();
        let resp = test::call_service(&app, req).await;
        assert_eq!(resp.status(), 500);

        let count = API_REQUESTS_TOTAL
            .with_label_values(&["GET", "/v1/fail", "500"])
            .get();
        assert!(count >= 1, "expected counter >= 1, got {count}");
    }
}
