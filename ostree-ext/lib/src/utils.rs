pub(crate) trait ResultExt<T, E: std::fmt::Display> {
    /// Return the Ok value unchanged.  In the err case, log it, and call the closure to compute the default
    fn log_err_or_else<F>(self, default: F) -> T
    where
        F: FnOnce() -> T;
    /// Return the Ok value unchanged.  In the err case, log it, and return the default value
    fn log_err_default(self) -> T
    where
        T: Default;
}

impl<T, E: std::fmt::Display> ResultExt<T, E> for Result<T, E> {
    #[track_caller]
    fn log_err_or_else<F>(self, default: F) -> T
    where
        F: FnOnce() -> T,
    {
        match self {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("{e}");
                default()
            }
        }
    }

    #[track_caller]
    fn log_err_default(self) -> T
    where
        T: Default,
    {
        self.log_err_or_else(|| Default::default())
    }
}
