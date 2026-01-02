use std::sync::Arc;

use crate::chain::Runnable;

pub struct ParallelRunnable<I, O1, O2> {
    runnable1: Arc<dyn Runnable<I, O1>>,
    runnable2: Arc<dyn Runnable<I, O2>>,
}

impl<I, O1, O2> ParallelRunnable<I, O1, O2>
where
    I: Send + Sync + 'static,
    O1: Send + Sync + 'static,
    O2: Send + Sync + 'static,
{
    pub fn new<R1, R2>(r1: R1, r2: R2) -> Self
    where
        R1: Runnable<I, O1> + 'static,
        R2: Runnable<I, O2> + 'static,
    {
        Self {
            runnable1: Arc::new(r1),
            runnable2: Arc::new(r2),
        }
    }
}

#[async_trait::async_trait]
impl<I, O1, O2> Runnable<I, (Arc<O1>, Arc<O2>)> for ParallelRunnable<I, O1, O2>
where
    I: Send + Sync + 'static,
    O1: Send + Sync + 'static,
    O2: Send + Sync + 'static,
{
    async fn call(&self, input: Arc<I>) -> crate::Result<Arc<(Arc<O1>, Arc<O2>)>> {
        let f1 = self.runnable1.call(input.clone());
        let f2 = self.runnable2.call(input);

        let (res1, res2) = tokio::try_join!(f1, f2)?;
        Ok(Arc::new((res1, res2)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;
    use tokio::time::{Duration, sleep};

    // --- Mock Runnables ---

    struct SlowStep(u64);
    #[async_trait]
    impl Runnable<i32, i32> for SlowStep {
        async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
            sleep(Duration::from_millis(self.0)).await;
            Ok(Arc::new(*input + 10))
        }
    }

    struct ErrorStep;
    #[async_trait]
    impl Runnable<i32, i32> for ErrorStep {
        async fn call(&self, _input: Arc<i32>) -> crate::Result<Arc<i32>> {
            Err(crate::Error::Internal("Parallel Branch Failed".into()))
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn test_parallel_execution_success() {
        let r1 = SlowStep(10);
        let r2 = SlowStep(10);
        let parallel = ParallelRunnable::new(r1, r2);

        let input = Arc::new(5);
        let result = parallel.call(input).await.unwrap();

        let (out1, out2) = &*result;
        assert_eq!(**out1, 15);
        assert_eq!(**out2, 15);
    }

    #[tokio::test]
    async fn test_parallel_concurrency() {
        let r1 = SlowStep(100);
        let r2 = SlowStep(100);
        let parallel = ParallelRunnable::new(r1, r2);

        let start = tokio::time::Instant::now();
        let _ = parallel.call(Arc::new(0)).await.unwrap();
        let duration = start.elapsed();

        assert!(duration >= Duration::from_millis(100));
        assert!(duration < Duration::from_millis(180));
    }

    #[tokio::test]
    async fn test_parallel_error_propagation() {
        let r1 = SlowStep(10);
        let r2 = ErrorStep;
        let parallel = ParallelRunnable::new(r1, r2);

        let result = parallel.call(Arc::new(0)).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parallel_different_output_types() {
        struct IntStep;
        #[async_trait]
        impl Runnable<i32, i32> for IntStep {
            async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
                Ok(Arc::new(*input * 2))
            }
        }

        struct StringStep;
        #[async_trait]
        impl Runnable<i32, String> for StringStep {
            async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<String>> {
                Ok(Arc::new(input.to_string()))
            }
        }

        let parallel = ParallelRunnable::new(IntStep, StringStep);
        let result = parallel.call(Arc::new(10)).await.unwrap();

        let (out_int, out_str) = &*result;
        assert_eq!(**out_int, 20);
        assert_eq!(out_str.as_str(), "10");
    }

    #[tokio::test]
    async fn test_parallel_within_chain() {
        use crate::chain::Chain;

        // Definimos una cadena que usa el ParallelRunnable
        let parallel = ParallelRunnable::new(SlowStep(0), SlowStep(0));

        let mut chain = Chain::new()
            .add_step("parallel_step", parallel)
            .set_input(100);

        let step_result = chain.next().await.unwrap().unwrap();

        let typed = step_result.downcast::<i32, (Arc<i32>, Arc<i32>)>().unwrap();
        let (res1, res2) = &*typed.output;

        assert_eq!(**res1, 110);
        assert_eq!(**res2, 110);
    }
}
