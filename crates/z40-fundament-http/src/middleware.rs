//! The tower service behind the layers of this crate. Each layer is a small [`Hook`]: it looks at the
//! request (and may answer it right away, e.g. the CSRF guard) and then adjusts the response. One
//! service type keeps the `poll_ready`/`call` plumbing in one place.
//!
//! Apps do not use this module directly; they add the layers with axum's `Router::layer`, which hands
//! every layer an axum [`Request`] and expects an axum [`Response`].

use axum::extract::Request;
use axum::response::Response;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use tower::Service;

mod sealed {
    /// Keeps [`super::Hook`] closed: only this crate implements it.
    pub trait Sealed {}
}
pub(crate) use sealed::Sealed;

/// Request and response logic of one layer of this crate. Sealed: implemented by the layers here only.
pub trait Hook: Sealed + Clone + Send + Sync + 'static {
    /// What the request side hands to the response side.
    type Carry: Send + 'static;

    /// Runs before the inner service. `Err` answers the request without calling the inner service.
    // The `Err` is the finished answer, built only on rejection; boxing it would gain nothing.
    #[allow(clippy::result_large_err)]
    fn on_request(&self, req: &mut Request) -> Result<Self::Carry, Response>;

    /// Runs on the response of the inner service.
    fn on_response(&self, carry: Self::Carry, res: &mut Response);
}

/// The service a layer of this crate wraps around the inner service.
#[derive(Debug, Clone)]
pub struct HookService<S, H> {
    inner: S,
    hook: H,
}

impl<S, H> HookService<S, H> {
    /// Wraps `inner` with `hook`.
    pub(crate) fn new(inner: S, hook: H) -> Self {
        Self { inner, hook }
    }
}

/// The future of [`HookService`].
pub type HookFuture<E> = Pin<Box<dyn Future<Output = Result<Response, E>> + Send>>;

impl<S, H> Service<Request> for HookService<S, H>
where
    S: Service<Request, Response = Response> + Send + 'static,
    S::Future: Send + 'static,
    H: Hook,
{
    type Response = Response;
    type Error = S::Error;
    type Future = HookFuture<S::Error>;

    /// Ready when the inner service is.
    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    /// Request hook, inner service, response hook.
    fn call(&mut self, mut req: Request) -> Self::Future {
        match self.hook.on_request(&mut req) {
            Err(answer) => Box::pin(async move { Ok(answer) }),
            Ok(carry) => {
                let fut = self.inner.call(req);
                let hook = self.hook.clone();
                Box::pin(async move {
                    let mut res = fut.await?;
                    hook.on_response(carry, &mut res);
                    Ok(res)
                })
            }
        }
    }
}

/// Implements `tower::Layer` for a [`Hook`] type (the layer is the configuration itself).
macro_rules! hook_layer {
    ($ty:ty) => {
        impl<S> tower::Layer<S> for $ty {
            type Service = $crate::middleware::HookService<S, $ty>;

            /// Wraps `inner` with this layer.
            fn layer(&self, inner: S) -> Self::Service {
                $crate::middleware::HookService::new(inner, self.clone())
            }
        }
    };
}
pub(crate) use hook_layer;
