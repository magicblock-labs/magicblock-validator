//! Codec and stream types for length-prefixed bincode framing.

use bytes::{BufMut, BytesMut};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use crate::{
    error::{Error, Result},
    proto::Message,
};

/// Encodes `Message` with 4-byte LE length prefix.
pub struct MessageEncoder;

pub(crate) type InputStream<IO> = FramedRead<IO, LengthDelimitedCodec>;
pub(crate) type OutputStream<IO> = FramedWrite<IO, MessageEncoder>;

const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

impl tokio_util::codec::Encoder<Message> for MessageEncoder {
    type Error = Error;

    fn encode(&mut self, msg: Message, dst: &mut BytesMut) -> Result<()> {
        let start = dst.len();
        dst.put_u32_le(0);
        bincode::serialize_into(dst.writer(), &msg)?;
        let len = dst.len() - start - 4;
        if len > MAX_FRAME_SIZE {
            dst.truncate(start);
            return Err(Box::new(bincode::ErrorKind::SizeLimit))?;
        }
        dst[start..start + 4].copy_from_slice(&(len as u32).to_le_bytes());
        Ok(())
    }
}

/// Receives messages from an async stream (max frame: 64MB).
pub struct Receiver<IO> {
    inner: InputStream<IO>,
}

impl<IO: AsyncRead + Unpin> Receiver<IO> {
    pub fn new(io: IO) -> Self {
        let inner = LengthDelimitedCodec::builder()
            .little_endian()
            .max_frame_length(MAX_FRAME_SIZE)
            .length_field_type::<u32>()
            .new_read(io);
        Self { inner }
    }

    pub async fn recv(&mut self) -> Result<Message> {
        let frame =
            self.inner.next().await.ok_or(Error::ConnectionClosed)??;
        bincode::deserialize(&frame).map_err(Into::into)
    }
}

/// Sends messages to an async stream.
pub struct Sender<IO> {
    inner: OutputStream<IO>,
}

impl<IO: AsyncWrite + Unpin> Sender<IO> {
    pub fn new(io: IO) -> Self {
        Self {
            inner: FramedWrite::new(io, MessageEncoder),
        }
    }

    pub async fn send(&mut self, msg: Message) -> Result<()> {
        self.inner.send(msg).await?;
        Ok(())
    }
}
