use actix_web::{
    dev::{forward_ready, Service, ServiceRequest, ServiceResponse, Transform},
    Error,
};
use backtrace::Backtrace;
use futures_util::future::LocalBoxFuture;
use std::future::{ready, Ready};
use std::{env, sync::Arc};

#[derive(Clone)]
pub struct Rollbar {
    client: Option<Arc<rollbar::Client>>,
}

impl Rollbar {
    pub fn from_env() -> Self {
        let client = Self::client_from_env().map(Arc::new);
        Self { client }
    }

    pub fn client_from_env() -> Option<rollbar::Client> {
        let rollbar_key = env::var("ROLLBAR_KEY").ok()?;
        let rollbar_env = env::var("ROLLBAR_ENVIRONMENT")
            .ok()
            .unwrap_or_else(|| "development".to_owned());

        Some(rollbar::Client::new(rollbar_key, rollbar_env))
    }

    pub fn register_panic_hook(self) {
        let Some(client) = self.client else {
            return;
        };
        let panic_hook = ::std::panic::take_hook();
        std::panic::set_hook(Box::new(move |panic_info| {
            let backtrace = Backtrace::new();

            client
                .build_report()
                .from_panic(panic_info)
                .with_backtrace(&backtrace)
                .send();

            panic_hook(panic_info);
        }));
    }

    pub fn report_error<E: std::error::Error>(&self, error: &E, request: rollbar::Request) {
        if let Some(client) = &self.client {
            client
                .build_report()
                .from_error(&error)
                .with_request(request)
                .send();
        }
    }
}

impl<S, B> Transform<S, ServiceRequest> for Rollbar
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type InitError = ();
    type Transform = RollbarMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        let inner = self.clone();
        ready(Ok(RollbarMiddleware { service, inner }))
    }
}

pub struct RollbarMiddleware<S> {
    service: S,
    inner: Rollbar,
}

fn to_rollbar_request(request: &ServiceRequest) -> rollbar::Request {
    rollbar::Request {
        url: format!(
            "{}://{}{}",
            request.connection_info().scheme(),
            request.connection_info().host(),
            request.uri()
        ),
        method: request.method().to_string(),
        headers: request
            .headers()
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect(),
    }
}

impl<S, B> Service<ServiceRequest> for RollbarMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error>,
    S::Future: 'static,
    B: 'static,
{
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let inner = self.inner.clone();
        let detail = to_rollbar_request(&req);

        let fut = self.service.call(req);

        Box::pin(async move {
            let res = fut.await;
            match res {
                Err(e) => {
                    // Service errors
                    inner.report_error(&e, detail);
                    Err(e)
                }
                Ok(res) => {
                    // Response errors
                    if res.response().status().is_server_error() {
                        if let Some(e) = res.response().error() {
                            inner.report_error(e, detail);
                        }
                    }
                    Ok(res)
                }
            }
        })
    }
}
