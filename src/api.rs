use awful_aj::api::ask;
use awful_aj::{config::AwfulJadeConfig, template::ChatTemplate};
use rand::{rng, Rng};
use std::error::Error;
use std::fmt;
use std::time::{Duration as StdDuration, Instant};
use tokio::time::sleep;
use tracing::{error, info, instrument, warn};

/// Trait for async LLM interaction
pub trait AskAsync {
    type Response;
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>>;
}

/// Wrapper that adds exponential backoff retry logic to any AskAsync implementation
pub struct RetryAsk<T> {
    inner: T,
    max_retries: usize,
    base_delay: StdDuration,
    max_delay: StdDuration,
}

impl<T> RetryAsk<T>
where
    T: AskAsync,
{
    pub fn new(inner: T, max_retries: usize, base_delay: StdDuration) -> Self {
        Self {
            inner,
            max_retries,
            base_delay,
            max_delay: StdDuration::from_secs(30),
        }
    }
}

impl<T> fmt::Debug for RetryAsk<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetryAsk")
            .field("max_retries", &self.max_retries)
            .field("base_delay", &self.base_delay)
            .field("max_delay", &self.max_delay)
            .finish()
    }
}

impl<T> AskAsync for RetryAsk<T>
where
    T: AskAsync + fmt::Debug,
{
    type Response = T::Response;

    #[instrument(level = "info", skip_all)]
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        let total_t0 = Instant::now();
        let mut attempt = 0usize;

        loop {
            let attempt_t0 = Instant::now();
            match self.inner.ask(text).await {
                Ok(resp) => {
                    return Ok(resp);
                }
                Err(e) => {
                    attempt += 1;
                    let attempt_dt = attempt_t0.elapsed();
                    let total_dt = total_t0.elapsed();

                    if attempt > self.max_retries {
                        error!(
                            attempt,
                            max = self.max_retries,
                            elapsed_ms_attempt = attempt_dt.as_millis() as u128,
                            elapsed_ms_total = total_dt.as_millis() as u128,
                            error = %e,
                            "ask() exhausted retries"
                        );
                        return Err(e);
                    }

                    // backoff calc
                    let mut delay = self.base_delay.saturating_mul(1 << (attempt - 1));
                    if delay > self.max_delay {
                        delay = self.max_delay;
                    }
                    let jitter_ms: u64 = rng().random_range(0..=250);
                    let delay = delay + StdDuration::from_millis(jitter_ms);

                    warn!(
                        attempt,
                        max = self.max_retries,
                        elapsed_ms_attempt = attempt_dt.as_millis() as u128,
                        elapsed_ms_total = total_dt.as_millis() as u128,
                        ?delay,
                        error = %e,
                        "ask() attempt failed; backing off"
                    );
                    sleep(delay).await;
                }
            }
        }
    }
}

/// Wrapper around awful_aj::api::ask that implements AskAsync
#[derive(Debug)]
pub struct AskFnWrapper<'a> {
    pub config: &'a AwfulJadeConfig,
    pub template: &'a ChatTemplate,
}

impl<'a> AskAsync for AskFnWrapper<'a> {
    type Response = String;

    #[instrument(level = "info", skip_all)]
    async fn ask(&self, text: &str) -> Result<Self::Response, Box<dyn Error>> {
        let t0 = Instant::now();
        let res = ask(self.config, text.to_string(), self.template, None, None).await;
        let dt = t0.elapsed();

        match &res {
            Ok(_) => {}
            Err(e) => warn!(elapsed_ms = dt.as_millis() as u128, error = %e, "API call failed"),
        }
        res
    }
}

/// High-level function to call LLM with exponential backoff retry logic
#[instrument(level = "info", skip_all)]
pub async fn ask_with_backoff(
    config: &AwfulJadeConfig,
    article: &String,
    template: &ChatTemplate,
) -> Result<String, Box<dyn Error>> {
    let t0 = Instant::now();
    let client = AskFnWrapper { config, template };
    let api = RetryAsk::new(client, 5, StdDuration::from_secs(1));
    let res = api.ask(article).await;
    let dt = t0.elapsed();

    match &res {
        Ok(_) => info!(
            elapsed_ms_total = dt.as_millis() as u128,
            "ask_with_backoff succeeded"
        ),
        Err(e) => {
            error!(elapsed_ms_total = dt.as_millis() as u128, error = %e, "ask_with_backoff failed")
        }
    }
    res
}
