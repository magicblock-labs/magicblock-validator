use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};

use http_body_util::BodyExt;
use hyper::body::{Body, Bytes, Frame, Incoming, SizeHint};
use hyper::Request;
use json::Serialize;
use json::{Object, Value};

use crate::RpcResult;

use super::RpcError;

pub(super) enum Data {
    Empty,
    SingleChunk(Bytes),
    MultiChunk(Vec<u8>),
}

pub(super) fn parse_body(body: Data) -> RpcResult<Object> {
    let body = match &body {
        Data::Empty => {
            return Err(RpcError::invalid_request("missing request body"));
        }
        Data::SingleChunk(slice) => slice.as_ref(),
        Data::MultiChunk(vec) => vec.as_ref(),
    };
    let body = json::from_slice::<Value>(body)?;
    body.into_object()
        .ok_or_else(|| RpcError::invalid_request("missing request params"))
}

pub(super) async fn extract_bytes(
    request: Request<Incoming>,
) -> RpcResult<Data> {
    let mut request = request.into_body();
    let mut data = Data::Empty;
    while let Some(next) = request.frame().await {
        let Ok(chunk) = next?.into_data() else {
            continue;
        };
        match &mut data {
            Data::Empty => data = Data::SingleChunk(chunk),
            Data::SingleChunk(first) => {
                let mut buffer = Vec::with_capacity(first.len() + chunk.len());
                buffer.extend_from_slice(first);
                buffer.extend_from_slice(&chunk);
                data = Data::MultiChunk(buffer);
            }
            Data::MultiChunk(buffer) => {
                buffer.extend_from_slice(&chunk);
            }
        }
    }
    Ok(data)
}

pub(crate) struct JsonBody(Vec<u8>);

impl<S: Serialize> From<S> for JsonBody {
    fn from(value: S) -> Self {
        let serialized = json::to_vec(&value)
            .expect("json serializiation into vec is infallible");
        Self(serialized)
    }
}

impl Body for JsonBody {
    type Data = Bytes;
    type Error = Infallible;

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.0.len() as u64)
    }

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        if !self.0.is_empty() {
            let s = std::mem::take(&mut self.0);
            Poll::Ready(Some(Ok(Frame::data(s.into()))))
        } else {
            Poll::Ready(None)
        }
    }
}
