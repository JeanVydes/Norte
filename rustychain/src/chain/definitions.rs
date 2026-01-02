use std::{any::Any, marker::PhantomData, sync::Arc};

use futures_core::future::BoxFuture;

use crate::chain::{
    StepResult,
    step::{Runnable, RunnableWrapper},
};

pub type IO = Arc<dyn Any + Send + Sync>;

#[derive(Clone)]
pub struct Chain<I, O> {
    steps: Vec<(String, RunnableWrapper)>,
    current_index: usize,
    current_value: Option<IO>,
    _phantom: PhantomData<(I, O)>,
}

impl<I, O> Chain<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    pub fn new() -> Self {
        Self {
            steps: Vec::new(),
            current_index: 0,
            current_value: None,
            _phantom: PhantomData,
        }
    }

    pub fn new_with_steps(steps: Vec<(String, RunnableWrapper)>) -> Self {
        Self {
            steps,
            current_index: 0,
            current_value: None,
            _phantom: PhantomData,
        }
    }

    pub fn with_input(&mut self, input: Arc<I>) -> &mut Self {
        self.current_value = Some(input);
        self.current_index = 0;
        self
    }

    // Each step, the chain is consumed and produces a new chain with updated output type
    // Transformation: Chain<I, O> -> Chain<I, O2>
    pub fn add_step<R, O2>(mut self, name: impl Into<String>, runnable: R) -> Chain<I, O2>
    where
        // Type Constraint Enforcement: The Runnable must take the current Chain output (O)
        // as its input and produce the new Chain output (O2).
        R: Runnable<O, O2> + 'static,
        O2: Send + Sync + 'static,
    {
        let runnable = Arc::new(runnable);
        let name = name.into();

        self.steps.push((
            name,
            Arc::new(move |input: Arc<dyn Any + Send + Sync>| {
                let r = runnable.clone();
                Box::pin(async move {
                    // Downcast input from Arc<dyn Any> to the expected input type O.
                    // This is guaranteed to succeed at runtime because the compile-time
                    // Type State (R: Runnable<O, O2>) enforced that the previous step produced O.
                    let typed_input = input.downcast::<O>()?;

                    // Call the runnable with the downcasted input (O)
                    // and produce the new output (O2).
                    let before = r.before(typed_input).await?;
                    let output = r.call(before).await?;
                    let after = r.after(output).await?;
                    // Box the O2 result back into Arc<dyn Any> for storage.
                    Ok(after as Arc<dyn Any + Send + Sync>)
                })
            }),
        ));

        // Return a new Chain instance with the updated output type O2.
        // This is the core mechanism of the Type State Pattern, fixing the chain's
        // output type for all subsequent steps.
        Chain {
            steps: self.steps,
            current_index: self.current_index,
            current_value: self.current_value,
            _phantom: PhantomData,
        }
    }

    /// Set input and returns the chain
    pub fn set_input(mut self, input: I) -> Self {
        self.current_value = Some(Arc::new(input));
        self.current_index = 0;
        self
    }

    /// Executes the next step in the chain
    pub async fn next(&mut self) -> Option<crate::Result<StepResult>> {
        if self.current_index >= self.steps.len() {
            log::trace!("No more steps to execute in the chain.");
            return None;
        }

        log::trace!(
            "Preparing to execute step {} of {}.",
            self.current_index + 1,
            self.steps.len()
        );

        let current = self.current_value.take()?;
        let (name, step) = &self.steps[self.current_index];
        let input = current.clone();

        log::trace!(
            "[{} ({}/{})] ",
            name,
            self.current_index + 1,
            self.steps.len()
        );

        match step(current).await {
            Ok(result) => {
                let step_result = StepResult {
                    index: self.current_index,
                    name: name.clone(),
                    input,
                    output: result.clone(),
                };

                log::trace!(
                    "[{} ({}/{})] Step executed successfully. \n\n{}\n{:?}\n{}",
                    name,
                    self.current_index + 1,
                    self.steps.len(),
                    "----------------------------------------",
                    step_result.output,
                    "----------------------------------------"
                );

                self.current_index += 1;
                self.current_value = Some(result);

                Some(Ok(step_result))
            }
            Err(e) => {
                log::error!(
                    "[{} ({}/{})] Step execution failed: {}",
                    name,
                    self.current_index + 1,
                    self.steps.len(),
                    e
                );

                self.current_value = None;
                Some(Err(e))
            }
        }
    }

    /// Executes all remaining steps in the chain
    pub async fn run_remaining(&mut self) -> crate::Result<()> {
        log::trace!(
            "Running remaining steps from index {} of {}.",
            self.current_index + 1,
            self.steps.len()
        );

        while let Some(result) = self.next().await {
            result?;
        }
        Ok(())
    }

    /// Executes the entire chain from the beginning with the given input and returns the final output
    pub async fn run(mut self, input: I) -> crate::Result<Arc<O>> {
        log::trace!("Running the entire chain from the beginning.");

        self.current_value = Some(Arc::new(input));
        self.current_index = 0;

        for (index, (name, step)) in self.steps.iter().enumerate() {
            log::trace!(
                "[{} ({}/{})] Executing step...",
                name,
                index + 1,
                self.steps.len()
            );

            let current = self
                .current_value
                .take()
                .ok_or_else(|| crate::Error::StepError {
                    index,
                    step_name: Some(name.clone()),
                    source: Box::new(crate::Error::NotInput),
                })?;

            log::trace!("({}/{}) Step input prepared.", index + 1, self.steps.len());

            self.current_value = Some(step(current).await?);

            log::trace!(
                "[{} ({}/{})] Step executed successfully.",
                name,
                index + 1,
                self.steps.len()
            );

            self.current_index = index + 1;
        }

        log::trace!("All steps executed. Finalizing chain.");

        self.finalize()
    }

    /// Executes the entire chain from the beginning with the given input and an interceptor function
    pub async fn run_with_interceptor<F>(
        mut self,
        input: I,
        mut interceptor: F,
    ) -> crate::Result<Arc<O>>
    where
        F: FnMut(usize, &str, &dyn std::any::Any) -> BoxFuture<'static, crate::Result<()>>,
    {
        log::trace!("Running the entire chain with interceptor from the beginning.");
        self.current_value = Some(Arc::new(input));
        self.current_index = 0;

        for (index, (name, step)) in self.steps.iter().enumerate() {
            log::trace!(
                "[{} ({}/{})] Executing step...",
                name,
                index + 1,
                self.steps.len()
            );

            let current = self
                .current_value
                .take()
                .ok_or_else(|| crate::Error::StepError {
                    index,
                    step_name: Some(name.clone()),
                    source: Box::new(crate::Error::NotInput),
                })?;

            log::trace!(
                "[{} ({}/{})] Step input prepared.",
                name,
                index + 1,
                self.steps.len()
            );

            let result = step(current).await?;

            log::trace!(
                "[{} ({}/{})] Step executed successfully.",
                name,
                index + 1,
                self.steps.len()
            );

            log::trace!(
                "[{} ({}/{})] Executing interceptor...",
                name,
                index + 1,
                self.steps.len()
            );

            interceptor(index, name, result.as_ref()).await?;

            log::trace!(
                "[{} ({}/{})] Interceptor executed successfully.",
                name,
                index + 1,
                self.steps.len()
            );

            self.current_value = Some(result);
            self.current_index = index + 1;
        }

        self.finalize()
    }

    /// Finalizes the chain and returns the final output
    pub fn finalize(self) -> crate::Result<Arc<O>> {
        log::trace!("Finalizing the chain.");

        if self.current_index != self.steps.len() {
            return Err(crate::Error::ChainError {
                index: self.current_index,
                name: "".to_string(),
                source: Box::new(crate::Error::Internal("Chain not fully executed".into())),
            });
        }

        let final_value = self.current_value.ok_or_else(|| crate::Error::StepError {
            index: self.current_index,
            step_name: None,
            source: Box::new(crate::Error::StepError {
                index: self.current_index,
                step_name: None,
                source: Box::new(crate::Error::NotInput),
            }),
        })?;

        let output = final_value.downcast::<O>()?;

        Ok(output)
    }

    /// Get current value as Any
    pub fn current_value(&self) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        self.current_value.clone()
    }

    /// Resets the chain to the initial state with the given input
    pub fn reset(mut self, input: I) -> Self {
        self.current_index = 0;
        self.current_value = Some(Arc::new(input));
        self
    }

    /// Total number of steps in the chain
    pub fn step_count(&self) -> usize {
        self.steps.len()
    }

    /// Current step index
    pub fn current_step(&self) -> usize {
        self.current_index
    }

    /// Checks if there are more steps to execute
    pub fn has_next(&self) -> bool {
        self.current_index < self.steps.len()
    }
}

impl<I, O> Default for Chain<I, O>
where
    I: Send + Sync + 'static,
    O: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl<I, O> Runnable<I, O> for Chain<I, O>
where
    I: Clone + Send + Sync + 'static,
    O: Clone + Send + Sync + 'static,
{
    async fn call(&self, input: Arc<I>) -> crate::Result<Arc<O>> {
        let runner = self.clone();
        let result_arc = runner.run((*input).clone()).await?;
        Ok(result_arc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chain::step::Runnable;
    use async_trait::async_trait;
    use std::sync::Arc;

    // --- Mock Runnables para Testing ---

    struct AddOne;
    #[async_trait]
    impl Runnable<i32, i32> for AddOne {
        async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
            Ok(Arc::new(*input + 1))
        }
    }

    struct ToStringStep;
    #[async_trait]
    impl Runnable<i32, String> for ToStringStep {
        async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<String>> {
            Ok(Arc::new(input.to_string()))
        }
    }

    struct InterceptorStep;
    #[async_trait]
    impl Runnable<i32, i32> for InterceptorStep {
        async fn before(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
            // Multiplica por 2 antes de la ejecución
            Ok(Arc::new(*input * 2))
        }
        async fn call(&self, input: Arc<i32>) -> crate::Result<Arc<i32>> {
            Ok(Arc::new(*input + 5))
        }
        async fn after(&self, output: Arc<i32>) -> crate::Result<Arc<i32>> {
            // Suma 10 después de la ejecución
            Ok(Arc::new(*output + 10))
        }
    }

    // --- Tests ---

    #[tokio::test]
    async fn test_basic_chain_flow() {
        let chain = Chain::<i32, i32>::new()
            .add_step("step1", AddOne) // 10 + 1 = 11
            .add_step("step2", AddOne); // 11 + 1 = 12

        let result = chain.run(10).await.unwrap();
        assert_eq!(*result, 12);
    }

    #[tokio::test]
    async fn test_heterogeneous_chain() {
        let chain = Chain::<i32, i32>::new()
            .add_step("add", AddOne) // 5 + 1 = 6
            .add_step("string", ToStringStep); // "6"

        let result = chain.run(5).await.unwrap();
        assert_eq!(result.as_str(), "6");
    }

    #[tokio::test]
    async fn test_runnable_hooks_execution() {
        let chain = Chain::<i32, i32>::new().add_step("intercept", InterceptorStep);

        // Input: 10
        // Before: 10 * 2 = 20
        // Call: 20 + 5 = 25
        // After: 25 + 10 = 35
        let result = chain.run(10).await.unwrap();
        assert_eq!(*result, 35);
    }

    #[tokio::test]
    async fn test_chain_next_iteration() {
        let mut chain = Chain::<i32, i32>::new()
            .add_step("s1", AddOne)
            .add_step("s2", AddOne)
            .set_input(10);

        let r1 = chain.next().await.unwrap().unwrap();
        assert_eq!(r1.name, "s1");
        let out1 = r1.output.downcast::<i32>().unwrap();
        assert_eq!(*out1, 11);

        // Segundo paso
        let r2 = chain.next().await.unwrap().unwrap();
        assert_eq!(r2.name, "s2");
        let out2 = r2.output.downcast::<i32>().unwrap();
        assert_eq!(*out2, 12);

        assert!(chain.next().await.is_none());
    }

    #[tokio::test]
    async fn test_chain_reset() {
        let chain = Chain::<i32, i32>::new().add_step("s1", AddOne);

        let res1 = chain.clone().run(10).await.unwrap();
        assert_eq!(*res1, 11);

        let mut runner = chain.set_input(20);
        runner.run_remaining().await.unwrap();
        let res2 = runner.finalize().unwrap();
        assert_eq!(*res2, 21);
    }

    #[tokio::test]
    async fn test_run_with_interceptor() {
        let chain = Chain::<i32, i32>::new()
            .add_step("s1", AddOne)
            .add_step("s2", AddOne);

        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter_clone = counter.clone();

        let result = chain
            .run_with_interceptor(10, move |_idx, _name, _any| {
                let c = counter_clone.clone();
                Box::pin(async move {
                    c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(())
                })
            })
            .await
            .unwrap();

        assert_eq!(*result, 12);
        assert_eq!(counter.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn test_error_propagation() {
        struct ErrorStep;
        #[async_trait]
        impl Runnable<i32, i32> for ErrorStep {
            async fn call(&self, _input: Arc<i32>) -> crate::Result<Arc<i32>> {
                Err(crate::Error::Internal("Forced Error".into()))
            }
        }

        let chain = Chain::<i32, i32>::new().add_step("error_step", ErrorStep);

        let result = chain.run(10).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_type_state_pattern_safety() {
        let chain = Chain::<i32, i32>::new().add_step("step", ToStringStep);

        let res = chain.run(10).await.unwrap();
        assert_eq!(res.as_str(), "10");
    }
}
