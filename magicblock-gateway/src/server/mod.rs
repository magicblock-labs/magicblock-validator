use tokio::sync::Notify;

mod http;
mod websocket;

#[derive(Default)]
struct Shutdown(Notify);
impl Drop for Shutdown {
    fn drop(&mut self) {
        self.0.notify_last();
    }
}
