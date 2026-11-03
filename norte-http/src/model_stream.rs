use std::collections::VecDeque;

use futures_util::{StreamExt, stream};
use norte_core::{Error, ModelStream, ProviderError, ProviderErrorKind, Result, StreamEvent};

use crate::{SseEvent, SseStream};

/// Decodes vendor `SSE` events in order, preserving events after `Finish` such as usage.
///
/// EOF without a decoded [`StreamEvent::Finish`] yields one
/// [`ProviderErrorKind::Decode`] error. The first transport or decoding error is
/// emitted unchanged and terminates the stream, immediately dropping its source.
/// Completion is determined by decoded events, not a wire-specific sentinel.
pub fn decode_model_stream(
    sse: SseStream,
    decode: impl FnMut(&SseEvent) -> Result<Vec<StreamEvent>> + Send + 'static,
) -> ModelStream {
    let state = (Some(sse), decode, VecDeque::new(), false);
    Box::pin(stream::unfold(state, |mut state| async move {
        let (source, decode, pending, saw_finish) = &mut state;
        loop {
            if let Some(event) = pending.pop_front() {
                return Some((Ok(event), state));
            }

            let sse = source.as_mut()?;
            let result = match sse.next().await {
                Some(Ok(event)) => decode(&event),
                Some(Err(error)) => Err(error),
                None => {
                    *source = None;
                    if *saw_finish {
                        return None;
                    }
                    Err(Error::provider(ProviderError::new(
                        ProviderErrorKind::Decode,
                        "Model stream ended before a Finish event (unexpected EOF)",
                    )))
                }
            };
            match result {
                Ok(events) => {
                    *saw_finish |= events
                        .iter()
                        .any(|event| matches!(event, StreamEvent::Finish(_)));
                    pending.extend(events);
                }
                Err(error) => {
                    *source = None;
                    return Some((Err(error), state));
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use std::task::Poll;

    use futures_util::FutureExt;
    use norte_core::{FinishReason, Usage};

    use super::*;

    fn source(data: &[&str]) -> SseStream {
        Box::pin(stream::iter(
            data.iter()
                .map(|data| {
                    Ok(SseEvent {
                        data: (*data).to_owned(),
                        ..SseEvent::default()
                    })
                })
                .collect::<Vec<_>>(),
        ))
    }

    fn assert_truncated(result: Result<StreamEvent>) {
        let error = result.expect_err("EOF without Finish must fail");
        assert_eq!(error.provider_kind(), Some(ProviderErrorKind::Decode));
        assert!(error.to_string().contains("Finish"));
        assert!(error.to_string().contains("EOF"));
    }

    #[tokio::test]
    async fn empty_eof_emits_one_decode_error() {
        let mut model = decode_model_stream(source(&[]), |_| panic!("empty source"));
        assert_truncated(model.next().await.expect("EOF error"));
        assert!(model.next().await.is_none());
    }

    #[tokio::test]
    async fn partial_eof_preserves_events_then_emits_one_decode_error() {
        let expected = vec![
            StreamEvent::TextStart { index: 0 },
            StreamEvent::TextDelta {
                index: 0,
                text: "partial".to_owned(),
            },
        ];
        let batch = expected.clone();
        let mut model = decode_model_stream(source(&["ignored", "text"]), move |event| {
            Ok(if event.data == "text" {
                batch.clone()
            } else {
                Vec::new()
            })
        });
        for event in expected {
            assert_eq!(model.next().await.expect("event").expect("decoded"), event);
        }
        assert_truncated(model.next().await.expect("EOF error"));
        assert!(model.next().await.is_none());
    }

    #[tokio::test]
    async fn finish_preserves_trailing_usage_in_and_after_its_batch() {
        let finish = StreamEvent::Finish(FinishReason::Stop);
        let usage = StreamEvent::Usage(Usage::default());
        let expected = vec![finish.clone(), usage.clone(), usage.clone()];
        let model = decode_model_stream(source(&["finish", "usage"]), move |event| {
            Ok(if event.data == "finish" {
                vec![finish.clone(), usage.clone()]
            } else {
                vec![usage.clone()]
            })
        });
        let actual: Vec<_> = model.collect().await;
        assert_eq!(
            actual
                .into_iter()
                .collect::<Result<Vec<_>>>()
                .expect("complete stream"),
            expected
        );
    }

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn first_failure_drops_source_without_polling_pending_tail() {
        for transport_failure in [true, false] {
            let dropped = Arc::new(AtomicBool::new(false));
            let polls = Arc::new(AtomicUsize::new(0));
            let guard = DropFlag(dropped.clone());
            let tail_polls = polls.clone();
            let error = || {
                Error::provider(ProviderError::new(
                    ProviderErrorKind::ServerError,
                    "first failure",
                ))
            };
            let first = if transport_failure {
                Err(error())
            } else {
                Ok(SseEvent::default())
            };
            let sse = stream::iter([first]).chain(stream::poll_fn(move |_| {
                let _keep_alive = &guard;
                tail_polls.fetch_add(1, Ordering::SeqCst);
                Poll::Pending
            }));
            let mut model = decode_model_stream(Box::pin(sse), move |_| {
                assert!(!transport_failure, "transport errors bypass decode");
                Err(error())
            });
            let failure = model
                .next()
                .now_or_never()
                .expect("must not wait")
                .expect("failure event")
                .expect_err("first failure");
            assert_eq!(
                failure.provider_kind(),
                Some(ProviderErrorKind::ServerError)
            );
            assert!(failure.to_string().contains("first failure"));
            assert!(
                dropped.load(Ordering::SeqCst),
                "drop before yielding the error"
            );
            assert!(
                model
                    .next()
                    .now_or_never()
                    .expect("must not wait")
                    .is_none()
            );
            assert_eq!(polls.load(Ordering::SeqCst), 0);
        }
    }
}
