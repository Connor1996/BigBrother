use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::future::BoxFuture;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{config::ResolvedConfig, model::EventLevel};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub level: EventLevel,
    pub title: String,
    pub body: String,
}

impl Notification {
    pub fn new(level: EventLevel, title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            level,
            title: title.into(),
            body: body.into(),
        }
    }
}

pub trait NotificationSink: Send + Sync {
    fn send(&self, notification: Notification) -> BoxFuture<'static, Result<()>>;
}

pub fn build_notification_sink(config: &ResolvedConfig) -> Result<Box<dyn NotificationSink>> {
    if let Some(feishu) = &config.notifications.feishu {
        return Ok(Box::new(FeishuWebhookSink::new(
            feishu.webhook_url.clone(),
            feishu.keyword.clone(),
            feishu.label.clone(),
            feishu.timeout_secs,
        )?));
    }

    Ok(Box::new(NoopNotificationSink))
}

pub struct NoopNotificationSink;

impl NotificationSink for NoopNotificationSink {
    fn send(&self, _notification: Notification) -> BoxFuture<'static, Result<()>> {
        Box::pin(async { Ok(()) })
    }
}

pub struct FeishuWebhookSink {
    client: Client,
    webhook_url: String,
    keyword: Option<String>,
    label: String,
}

impl FeishuWebhookSink {
    pub fn new(
        webhook_url: String,
        keyword: Option<String>,
        label: String,
        timeout_secs: u64,
    ) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1)))
            .build()
            .context("failed to build Feishu webhook client")?;

        Ok(Self {
            client,
            webhook_url,
            keyword,
            label,
        })
    }

    fn format_text(&self, notification: &Notification) -> String {
        let mut sections = Vec::new();

        if let Some(keyword) = &self.keyword {
            sections.push(keyword.clone());
        }

        sections.push(format!(
            "[{}][{}] {}",
            self.label,
            notification.level.label(),
            notification.title
        ));

        if !notification.body.trim().is_empty() {
            sections.push(notification.body.trim().to_owned());
        }

        sections.join("\n\n")
    }
}

impl NotificationSink for FeishuWebhookSink {
    fn send(&self, notification: Notification) -> BoxFuture<'static, Result<()>> {
        let client = self.client.clone();
        let webhook_url = self.webhook_url.clone();
        let text = self.format_text(&notification);

        Box::pin(async move {
            let response = client
                .post(&webhook_url)
                .json(&FeishuWebhookRequest::text(text))
                .send()
                .await
                .with_context(|| format!("failed to POST Feishu webhook at {webhook_url}"))?;
            let status = response.status();
            let body = response
                .text()
                .await
                .context("failed to read Feishu webhook response body")?;

            if !status.is_success() {
                return Err(anyhow!(
                    "Feishu webhook returned HTTP {}: {}",
                    status,
                    body.trim()
                ));
            }

            if body.trim().is_empty() {
                return Ok(());
            }

            let Ok(parsed) = serde_json::from_str::<FeishuWebhookResponse>(&body) else {
                return Ok(());
            };

            if !parsed.is_success() {
                return Err(anyhow!(
                    "Feishu webhook rejected notification: {}",
                    parsed.message()
                ));
            }

            Ok(())
        })
    }
}

#[derive(Debug, Serialize)]
struct FeishuWebhookRequest {
    msg_type: &'static str,
    content: FeishuWebhookTextContent,
}

impl FeishuWebhookRequest {
    fn text(text: String) -> Self {
        Self {
            msg_type: "text",
            content: FeishuWebhookTextContent { text },
        }
    }
}

#[derive(Debug, Serialize)]
struct FeishuWebhookTextContent {
    text: String,
}

#[derive(Debug, Deserialize)]
struct FeishuWebhookResponse {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default, rename = "StatusCode")]
    status_code: Option<i64>,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default, rename = "StatusMessage")]
    status_message: Option<String>,
}

impl FeishuWebhookResponse {
    fn is_success(&self) -> bool {
        self.code.or(self.status_code).unwrap_or(0) == 0
    }

    fn message(&self) -> String {
        self.msg
            .clone()
            .or_else(|| self.status_message.clone())
            .unwrap_or_else(|| "unknown Feishu webhook error".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feishu_text_format_includes_keyword_label_and_body() {
        let sink = FeishuWebhookSink::new(
            "https://example.com/hook".to_owned(),
            Some("Symphony".to_owned()),
            "connor-mbp".to_owned(),
            5,
        )
        .expect("sink should build");
        let text = sink.format_text(&Notification::new(
            EventLevel::Error,
            "agent run failed for openai/symphony#42",
            "Summary: CI failure handling failed",
        ));

        assert_eq!(
            text,
            "Symphony\n\n[connor-mbp][error] agent run failed for openai/symphony#42\n\nSummary: CI failure handling failed"
        );
    }

    #[test]
    fn webhook_response_accepts_both_code_shapes() {
        let code_shape: FeishuWebhookResponse =
            serde_json::from_str(r#"{"code":0,"msg":"success"}"#).expect("parse");
        let status_shape: FeishuWebhookResponse =
            serde_json::from_str(r#"{"StatusCode":0,"StatusMessage":"success"}"#).expect("parse");

        assert!(code_shape.is_success());
        assert!(status_shape.is_success());
    }
}
