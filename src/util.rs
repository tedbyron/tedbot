//! Utility items.

pub trait TraceResult {
    fn or_trace(self);
    fn trace_err(self) -> Self;
}

impl<T, E> TraceResult for std::result::Result<T, E>
where
    E: std::fmt::Debug,
{
    fn or_trace(self) {
        if let Err(ref e) = self {
            tracing::error!("{:?}", e);
        }
    }

    fn trace_err(self) -> Self {
        if let Err(ref e) = self {
            tracing::error!("{:?}", e);
        }

        self
    }
}
