//! Runs one async HTTP exchange to completion from a synchronous command. Each
//! call gets a scratch thread with its own runtime, because the CLI's tokio
//! runtime is already driving the synchronous command that asked for it.

use std::{future::Future, thread};

use anyhow::{anyhow, Context, Result};

pub(crate) fn call<T, F, Fut>(work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T>>,
{
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to start the HTTP runtime")?;
        runtime.block_on(work())
    });
    worker
        .join()
        .map_err(|_| anyhow!("the HTTP worker panicked"))?
}
