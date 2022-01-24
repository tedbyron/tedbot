//! Utility items.

pub trait TraceResult {
    fn trace_err(self);
}

impl<T, E> TraceResult for std::result::Result<T, E>
where
    E: std::fmt::Debug,
{
    fn trace_err(self) {
        if let Err(ref e) = self {
            tracing::error!("{:?}", e);
        }
    }
}
