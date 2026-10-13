//! Branch syntax projects one typed event future into the caller's selector.

/// Selects one LIVE event by pattern alongside ordinary asynchronous branches.
/// The caller supplies a selector such as `tokio::select`; the graph future is
/// evaluated once and all graph cases share its single delivery consumer.
/// Cancellation and pending conflict ownership belong to `next_event`.
///
/// ```ignore
/// ferros_grynn::live_graph_select! {
///     select = tokio::select;
///     event = live.next_event() => {
///         Ok(LiveGraphNext::Conflict(conflict)) => { show(conflict); },
///         Ok(LiveGraphNext::Updated { revision }) => { render(revision); },
///         Ok(other) => { handle(other); },
///         Err(error) => { report(error); },
///     },
///     _ = cancellation.cancelled() => { break; },
/// }
/// ```
#[macro_export]
macro_rules! live_graph_select {
	(select = $selector:path; event = $event:expr => { $($pattern:pat => $body:expr),+ $(,)? } $(, $($other:tt)*)?) => {
		$selector! {
			outcome = $event => match outcome { $($pattern => $body),+ },
			$($($other)*)?
		}
	};
}

#[cfg(test)]
mod tests {
	use std::{
		future::Future,
		pin::pin,
		sync::Arc,
		task::{Context, Poll, Wake, Waker},
	};

	/// A selector stub isolates syntax/hygiene from an executor dependency.
	macro_rules! select_one {
		($value:pat = $future:expr => $body:expr $(,)?) => {{
			let $value = $future.await;
			$body
		}};
	}

	#[test]
	fn branch_macro_evaluates_one_future_once_and_preserves_patterns() {
		struct Noop;
		impl Wake for Noop {
			fn wake(self: Arc<Self>) {}
		}
		let waker = Waker::from(Arc::new(Noop));
		let mut context = Context::from_waker(&waker);
		let mut calls = 0;
		let outcome = 99;
		let operation = async {
			crate::live_graph_select! {
				select = select_one;
				event = { calls += 1; std::future::ready(Ok::<_, ()>(7)) } => {
					Ok(7) => outcome,
					Ok(_) => 0,
					Err(_) => 1,
				},
			}
		};
		{
			let mut operation = pin!(operation);
			assert_eq!(operation.as_mut().poll(&mut context), Poll::Ready(99));
		}
		assert_eq!(calls, 1);
	}
}
