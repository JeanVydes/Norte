use std::{any::Any, sync::Arc};

use futures_core::future::BoxFuture;

#[async_trait::async_trait]
pub trait Runnable<I, O>: Send + Sync + 'static
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    async fn call(&self, input: Arc<I>) -> crate::Result<Arc<O>>;

    // interceptors
    async fn before(&self, _input: Arc<I>) -> crate::Result<Arc<I>> {
        Ok(_input)
    }

    async fn after(&self, _output: Arc<O>) -> crate::Result<Arc<O>> {
        Ok(_output)
    }
}

pub type RunnableWrapper = Arc<
    dyn Fn(
            Arc<dyn Any + Send + Sync>,
        ) -> BoxFuture<'static, crate::Result<Arc<dyn Any + Send + Sync>>>
        + Send
        + Sync,
>;

#[macro_export]
macro_rules! impl_runnable {
    ($type:ty, $input:ty, $output:ty) => {
        impl Runnable<$input, $output> for $type {
            fn call(&self, input: $input) -> BoxFuture<'_, $crate::Result<$output>> {
                Box::pin(async move { self.run_impl(input).await })
            }
        }
    };
}

pub struct StepResult {
    pub index: usize,
    pub name: String,
    pub input: Arc<dyn Any + Send + Sync>,
    pub output: Arc<dyn Any + Send + Sync>,
}

impl StepResult {
    pub fn downcast<I, O>(self) -> crate::Result<TypedStepResult<I, O>>
    where
        I: Send + Sync + 'static,
        O: Send + Sync + 'static,
    {
        let input = self.input.downcast::<I>()?;
        let output = self.output.downcast::<O>()?;
        Ok(TypedStepResult {
            index: self.index,
            name: self.name,
            input,
            output,
        })
    }
}

pub struct TypedStepResult<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    pub index: usize,
    pub name: String,
    pub input: Arc<I>,
    pub output: Arc<O>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // --- Mocks para Pruebas Exigentes ---

    struct ComplexState {
        counter: Arc<AtomicUsize>,
    }

    struct HookTester {
        state: ComplexState,
    }

    #[async_trait]
    impl Runnable<i32, String> for HookTester {
        async fn before(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
            self.state.counter.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(*input * 2))
        }

        async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<String>> {
            self.state.counter.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(input.to_string()))
        }

        async fn after(&self, output: Arc<String>) -> crate::Result<Arc<String>> {
            self.state.counter.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(format!("final_{}", output)))
        }
    }

    // --- Tests StepResult  and Downcasting ---

    #[test]
    fn test_step_result_successful_downcast() {
        let input = Arc::new(42i32);
        let output = Arc::new("hello".to_string());

        let result = StepResult {
            index: 0,
            name: "test_step".into(),
            input: input.clone() as Arc<dyn Any + Send + Sync>,
            output: output.clone() as Arc<dyn Any + Send + Sync>,
        };

        let typed = result
            .downcast::<i32, String>()
            .expect("Should downcast correctly");

        assert_eq!(*typed.input, 42);
        assert_eq!(*typed.output, "hello");
        assert_eq!(typed.name, "test_step");
    }

    #[test]
    fn test_step_result_wrong_type_fails_gracefully() {
        let result = StepResult {
            index: 1,
            name: "fail_step".into(),
            input: Arc::new(10i32),
            output: Arc::new(20i32),
        };

        let err = result.downcast::<i32, String>();
        assert!(err.is_err());
    }

    // --- Tests lifecycle hooks ---

    #[tokio::test]
    async fn test_runnable_hooks_order_and_transformation() {
        let counter = Arc::new(AtomicUsize::new(0));
        let runner = HookTester {
            state: ComplexState {
                counter: counter.clone(),
            },
        };

        let input = Arc::new(5);

        // Ejecución manual del ciclo que haría la Chain
        let b = runner.before(input).await.unwrap();
        assert_eq!(*b, 10, "Before hook should have doubled the input");

        let c = runner.call(b).await.unwrap();
        assert_eq!(*c, "10", "Call should have stringified the doubled input");

        let a = runner.after(c).await.unwrap();
        assert_eq!(*a, "final_10", "After hook should have added prefix");

        assert_eq!(
            counter.load(Ordering::SeqCst),
            3,
            "All 3 hooks must have executed"
        );
    }

    // --- Tests (Any + Send + Sync) ---

    #[tokio::test]
    async fn test_runnable_wrapper_concurrency() {
        let wrapper: RunnableWrapper = Arc::new(|input| {
            Box::pin(async move {
                let val = input.downcast::<i32>().unwrap();
                Ok(Arc::new(*val + 1) as Arc<dyn Any + Send + Sync>)
            })
        });

        let mut handles = vec![];
        for i in 0..10 {
            let w = wrapper.clone();
            handles.push(tokio::spawn(async move {
                let input = Arc::new(i);
                let res = w(input).await.unwrap();
                let out = res.downcast::<i32>().unwrap();
                *out
            }));
        }

        for (i, handle) in handles.into_iter().enumerate() {
            let res = handle.await.unwrap();
            assert_eq!(res, (i as i32) + 1);
        }
    }

    #[tokio::test]
    async fn test_default_hooks_pass_through() {
        struct MinimalRunner;
        #[async_trait]
        impl Runnable<i32, i32> for MinimalRunner {
            async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
                Ok(input)
            }
        }

        let runner = MinimalRunner;
        let input = Arc::new(100);

        let b = runner.before(input.clone()).await.unwrap();
        let a = runner.after(input.clone()).await.unwrap();

        assert!(Arc::ptr_eq(&input, &b));
        assert!(Arc::ptr_eq(&input, &a));
    }
}
