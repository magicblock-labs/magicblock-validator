use std::sync::Arc;

use crate::grpc_messages::{Message, MessageBlockMeta};

pub type GeyserMessage = Arc<Message>;
pub type GeyserMessages = Arc<Vec<GeyserMessage>>;
pub type GeyserMessageBlockMeta = Arc<MessageBlockMeta>;
