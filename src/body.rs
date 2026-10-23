#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{stream, FutureExt};
    use std::cell::Cell;

    #[test]
    fn joins_chunks_up_to_the_limit() {
        let chunks = stream::iter([Ok(vec![1, 2]), Ok(vec![3])]);
        let result = read_limited(chunks, 3).now_or_never().unwrap().unwrap();
        assert_eq!(result, vec![1, 2, 3]);
    }

    #[test]
    fn oversized_body_stops_before_reading_the_rest() {
        let consumed = Cell::new(0);
        let chunks = stream::iter((0..100).map(|_| {
            consumed.set(consumed.get() + 1);
            Ok(vec![0; 4])
        }));
        let result = read_limited(chunks, 5).now_or_never().unwrap();
        assert!(matches!(result, Err(ReadError::TooLarge)));
        assert_eq!(consumed.get(), 2);
    }

    #[test]
    fn propagates_stream_errors_without_returning_partial_data() {
        let chunks = stream::iter([Ok(vec![1]), Err(worker::Error::RustError("broken stream".into()))]);
        let result = read_limited(chunks, 5).now_or_never().unwrap();
        assert!(matches!(result, Err(ReadError::Stream(_))));
    }

    #[test]
    fn accepts_an_empty_body_with_a_zero_limit() {
        let result = read_limited(stream::empty(), 0).now_or_never().unwrap().unwrap();
        assert!(result.is_empty());
    }
}
