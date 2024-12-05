use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct UnsubscribeTokens {
    tokens: Arc<Mutex<HashMap<u64, CancellationToken>>>,
}

impl UnsubscribeTokens {
    pub fn new() -> Self {
        Self {
            tokens: Arc::<Mutex<HashMap<u64, CancellationToken>>>::default(),
        }
    }

    pub fn add(&self, id: u64) -> CancellationToken {
        let token = CancellationToken::new();
        let mut tokens = self.tokens.lock().unwrap();
        tokens.insert(id, token.clone());
        token
    }

    pub fn unsubscribe(&self, id: u64) {
        let mut tokens = self.tokens.lock().unwrap();
        if let Some(token) = tokens.remove(&id) {
            token.cancel();
        }
    }
}
