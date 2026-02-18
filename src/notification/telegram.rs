use async_trait::async_trait;

use super::filter::should_notify;
use super::formatter::format_notification;
use super::notifier::Notifier;
use crate::error::{OrchestratorError, Result};
use crate::model::message::Message;

/// Telegram notification backend.
#[derive(Debug)]
pub struct TelegramNotifier {
    pub bot_token: String,
    pub chat_ids: Vec<String>,
    client: reqwest::Client,
}

impl TelegramNotifier {
    pub fn new(bot_token: String, chat_ids: Vec<String>) -> Self {
        Self {
            bot_token,
            chat_ids,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Notifier for TelegramNotifier {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn notify(&self, message: &Message) -> Result<()> {
        if !should_notify(&message.frontmatter.intent) {
            return Ok(());
        }

        let text = format_notification(message);
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);

        for chat_id in &self.chat_ids {
            let resp = self
                .client
                .post(&url)
                .form(&[("chat_id", chat_id.as_str()), ("text", &text)])
                .send()
                .await
                .map_err(|e| {
                    OrchestratorError::Notification(format!(
                        "telegram send failed for chat {}: {}",
                        chat_id, e
                    ))
                })?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                return Err(OrchestratorError::Notification(format!(
                    "telegram API error for chat {}: {} - {}",
                    chat_id, status, body
                )));
            }
        }

        Ok(())
    }
}
