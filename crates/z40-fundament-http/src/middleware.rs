//! The tower service behind the layers of this crate. Each layer is a small [`Hook`]: it looks at the
//! request (and may answer it right away, e.g. the CSRF guard) and then adjusts the response. One
//! service type keeps the `poll_ready`/`call` plumbing in one place.
//!
//! Apps do not use this module directly; they add the layers with axum's `Router::layer`, which hands
//! every layer an axum [`Request`] and expects an axum [`Response`].
//!
//! **Readiness.** `poll_ready` reserves capacity of the inner service (e.g. a permit of a concurrency
//! limit). `call` therefore takes the ready inner service out and leaves a fresh clone behind (the usual
//! tower pattern): a request the hook answers itself drops the ready service with its reservation instead
//! of keeping it, and an accepted request runs on the service that was polled ready.

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
    S: Service<Request, Response = Response> + Clone + Send + 'static,
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

    /// Request hook, inner service, response hook. The inner service polled ready is taken out (a
    /// clone stays for the next `poll_ready`); on rejection it is dropped, releasing its reservation.
    fn call(&mut self, mut req: Request) -> Self::Future {
        let clone = self.inner.clone();
        let mut ready = std::mem::replace(&mut self.inner, clone);
        match self.hook.on_request(&mut req) {
            Err(answer) => {
                drop(ready);
                Box::pin(async move { Ok(answer) })
            }
            Ok(carry) => {
                let fut = ready.call(req);
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

#[cfg(test)]
mod tests {
    use crate::csrf::Csrf;
    use axum::body::Body;
    use axum::extract::Request;
    use axum::http::{Method, StatusCode};
    use axum::response::Response;
    use std::convert::Infallible;
    use std::task::{Context, Poll, Waker};
    use tower::limit::ConcurrencyLimit;
    use tower::{Layer, Service, service_fn};

    /// Polls `svc` for readiness once, without waiting.
    fn ready_now<S: Service<Request>>(svc: &mut S) -> bool {
        let mut cx = Context::from_waker(Waker::noop());
        matches!(svc.poll_ready(&mut cx), Poll::Ready(Ok(())))
    }

    fn request(method: Method) -> Request {
        Request::builder()
            .method(method)
            .uri("/x")
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn a_rejected_request_releases_the_readiness_of_the_inner_service() {
        let ok =
            service_fn(|_: Request| async { Ok::<_, Infallible>(Response::new(Body::empty())) });
        let csrf = Csrf::header("x-csrf", "1").unwrap();
        let mut first = csrf.layer(ConcurrencyLimit::new(ok, 1));
        let mut second = first.clone();

        assert!(ready_now(&mut first), "the only permit");
        assert!(!ready_now(&mut second), "held by the first");
        let rejected = first.call(request(Method::POST)).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
        assert!(ready_now(&mut second), "the rejection gave the permit back");

        let accepted = second.call(request(Method::GET));
        assert!(!ready_now(&mut first), "taken by the accepted request");
        assert_eq!(accepted.await.unwrap().status(), StatusCode::OK);
        assert!(ready_now(&mut first), "released when the request is done");
    }
}
