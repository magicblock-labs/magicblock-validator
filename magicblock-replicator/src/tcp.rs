//! TCP transport utilities.

use std::{io, net::SocketAddr};

use tokio::net::{
    tcp::{OwnedReadHalf, OwnedWriteHalf},
    TcpStream,
};

use crate::connection::{Receiver, Sender};

pub type TcpReceiver = Receiver<OwnedReadHalf>;
pub type TcpSender = Sender<OwnedWriteHalf>;

/// Connects to a primary at `addr`, returning (sender, receiver).
pub async fn connect(addr: SocketAddr) -> io::Result<(TcpSender, TcpReceiver)> {
    TcpStream::connect(addr).await.map(split)
}

/// Splits a TCP stream into sender and receiver halves.
pub fn split(stream: TcpStream) -> (TcpSender, TcpReceiver) {
    let (rx, tx) = stream.into_split();
    (Sender::new(tx), Receiver::new(rx))
}
