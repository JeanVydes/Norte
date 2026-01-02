use std::sync::Arc;

use crate::chain::Runnable;

pub struct ChainRouter<I, O> {
    classifier: Box<dyn Fn(Arc<I>) -> bool + Send + Sync>,
    left: Arc<dyn Runnable<I, O>>,
    right: Arc<dyn Runnable<I, O>>,
}

impl<I, O> ChainRouter<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    pub fn new<R1, R2, F>(left: R1, right: R2, classifier: F) -> Self
    where
        R1: Runnable<I, O> + 'static,
        R2: Runnable<I, O> + 'static,
        F: Fn(Arc<I>) -> bool + Send + Sync + 'static,
    {
        Self {
            left: Arc::new(left),
            right: Arc::new(right),
            classifier: Box::new(classifier),
        }
    }
}

#[async_trait::async_trait]
impl<I, O> Runnable<I, O> for ChainRouter<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    async fn call(&self, input: Arc<I>) -> crate::Result<Arc<O>> {
        if (self.classifier)(input.clone()) {
            self.left.call(input).await
        } else {
            self.right.call(input).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Arc;

    // --- Mock Runnables ---

    struct LeftStep;
    #[async_trait]
    impl Runnable<String, String> for LeftStep {
        async fn call(&self, _input: Arc<String>) -> crate::Result<Arc<String>> {
            Ok(Arc::new("left_path".to_string()))
        }
    }

    struct RightStep;
    #[async_trait]
    impl Runnable<String, String> for RightStep {
        async fn call(&self, _input: Arc<String>) -> crate::Result<Arc<String>> {
            Ok(Arc::new("right_path".to_string()))
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn test_router_logic_left() {
        let router = ChainRouter::new(LeftStep, RightStep, |input: Arc<String>| {
            input.contains("apple")
        });

        let input = Arc::new("i like apple".to_string());
        let result = router.call(input).await.unwrap();

        assert_eq!(*result, "left_path");
    }

    #[tokio::test]
    async fn test_router_logic_right() {
        let router = ChainRouter::new(LeftStep, RightStep, |input: Arc<String>| {
            input.contains("apple")
        });

        let input = Arc::new("i like orange".to_string());
        let result = router.call(input).await.unwrap();

        assert_eq!(*result, "right_path");
    }

    #[tokio::test]
    async fn test_router_complex_input() {
        #[derive(Debug, PartialEq)]
        struct Data {
            val: i32,
        }

        struct SuccessStep;
        #[async_trait]
        impl Runnable<Data, String> for SuccessStep {
            async fn call(&self, input: Arc<Data>) -> crate::Result<Arc<String>> {
                Ok(Arc::new(format!("Value is {}", input.val)))
            }
        }

        struct FailureStep;
        #[async_trait]
        impl Runnable<Data, String> for FailureStep {
            async fn call(&self, _input: Arc<Data>) -> crate::Result<Arc<String>> {
                Ok(Arc::new("Negative value".to_string()))
            }
        }

        let router = ChainRouter::new(SuccessStep, FailureStep, |input: Arc<Data>| input.val >= 0);

        let res_pos = router.call(Arc::new(Data { val: 10 })).await.unwrap();
        let res_neg = router.call(Arc::new(Data { val: -5 })).await.unwrap();

        assert_eq!(*res_pos, "Value is 10");
        assert_eq!(*res_neg, "Negative value");
    }

    #[tokio::test]
    async fn test_router_in_chain() {
        use crate::chain::Chain;

        let router = ChainRouter::new(LeftStep, RightStep, |input: Arc<String>| input.len() > 5);

        let mut chain = Chain::<String, String>::new()
            .add_step("routing", router)
            .set_input("short".to_string());

        let step_res = chain.next().await.unwrap().unwrap();
        let typed = step_res.downcast::<String, String>().unwrap();

        assert_eq!(*typed.output, "right_path");
    }
}
