use futures_util::{Stream, StreamExt};
use worker::{Error, Response, ResponseBody, Result};

#[derive(Debug)]
pub enum ReadError {
    TooLarge,
    Stream(Error),
}

impl From<ReadError> for Error {
    fn from(error: ReadError) -> Self {
        match error {
            ReadError::TooLarge => Error::RustError("Body exceeds size limit".into()),
            ReadError::Stream(error) => error,
        }
    }
}

/// Stop accumulating as soon as the limit is crossed, even without Content-Length.
pub async fn read_limited<S>(mut stream: S, limit: usize) -> std::result::Result<Vec<u8>, ReadError>
where
    S: Stream<Item = Result<Vec<u8>>> + Unpin,
{
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ReadError::Stream)?;
        if chunk.len() > limit.saturating_sub(bytes.len()) {
            return Err(ReadError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub async fn response_bytes(response: &mut Response, limit: usize) -> Result<Vec<u8>> {
    if let Some(length) = response.headers().get("Content-Length")? {
        if length.parse::<usize>().map(|n| n > limit).unwrap_or(true) {
            return Err(ReadError::TooLarge.into());
        }
    }
    match response.body() {
        ResponseBody::Empty => Ok(Vec::new()),
        ResponseBody::Body(bytes) if bytes.len() <= limit => Ok(bytes.clone()),
        ResponseBody::Body(_) => Err(ReadError::TooLarge.into()),
        ResponseBody::Stream(_) => Ok(read_limited(response.stream()?, limit).await?),
    }
}

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
