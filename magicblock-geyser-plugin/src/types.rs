use std::sync::Arc;

use tokio::sync::mpsc;

use crate::grpc_messages::{Message, MessageBlockMeta};

pub type GeyserMessage = Arc<Message>;
pub type GeyserMessages = Arc<Vec<GeyserMessage>>;
pub type GeyserMessageBlockMeta = Arc<MessageBlockMeta>;

pub type GeyserMessageSender = mpsc::UnboundedSender<GeyserMessage>;
pub type GeyserMessageReceiver = mpsc::UnboundedReceiver<GeyserMessage>;
pub fn geyser_message_channel() -> (GeyserMessageSender, GeyserMessageReceiver)
{
    mpsc::unbounded_channel()
}
