use async_trait::async_trait;
use std::fmt::Debug;

use crate::error::Result;
use crate::model::message::Message;

/// Notification channel trait.
#[async_trait]
pub trait Notifier: Send + Sync + Debug {
    fn name(&self) -> &str;
    async fn notify(&self, message: &Message) -> Result<()>;
}
