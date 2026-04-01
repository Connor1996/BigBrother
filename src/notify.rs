use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures::future::BoxFuture;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::{
    config::{FeishuReceiveIdType, ResolvedConfig},
    model::EventLevel,
};

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
        return Ok(Box::new(FeishuAppBotSink::new(
            feishu.app_id.clone(),
            feishu.app_secret.clone(),
            feishu.receive_id.clone(),
            feishu.receive_id_type,
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

pub struct FeishuAppBotSink {
    client: Client,
    app_id: String,
    app_secret: String,
    receive_id: String,
    receive_id_type: FeishuReceiveIdType,
    label: String,
}

impl FeishuAppBotSink {
    pub fn new(
        app_id: String,
        app_secret: String,
        receive_id: String,
        receive_id_type: FeishuReceiveIdType,
        label: String,
        timeout_secs: u64,
    ) -> Result<Self> {
        let client =
            build_feishu_client(timeout_secs).context("failed to build Feishu app bot client")?;

        Ok(Self {
            client,
            app_id,
            app_secret,
            receive_id,
            receive_id_type,
            label,
        })
    }

    fn format_text(&self, notification: &Notification) -> String {
        format_feishu_text(&self.label, notification)
    }

    fn message_request_body(&self, text: String) -> Result<FeishuAppBotMessageRequest> {
        Ok(FeishuAppBotMessageRequest {
            receive_id: self.receive_id.clone(),
            msg_type: "text",
            content: serde_json::to_string(&FeishuTextContent { text })
                .context("failed to encode Feishu text message content")?,
        })
    }
}

impl NotificationSink for FeishuAppBotSink {
    fn send(&self, notification: Notification) -> BoxFuture<'static, Result<()>> {
        let client = self.client.clone();
        let app_id = self.app_id.clone();
        let app_secret = self.app_secret.clone();
        let receive_id_type = self.receive_id_type;
        let request_body = self.message_request_body(self.format_text(&notification));

        Box::pin(async move {
            let request_body = request_body?;
            let token_response = client
                .post("https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal")
                .json(&FeishuAppBotTokenRequest {
                    app_id: app_id.clone(),
                    app_secret: app_secret.clone(),
                })
                .send()
                .await
                .context("failed to request Feishu tenant_access_token")?;
            let token_status = token_response.status();
            let token_body = token_response
                .text()
                .await
                .context("failed to read Feishu tenant_access_token response body")?;
            if !token_status.is_success() {
                return Err(anyhow!(
                    "Feishu tenant_access_token request returned HTTP {}: {}",
                    token_status,
                    token_body.trim()
                ));
            }
            let token_payload: FeishuAppBotTokenResponse = serde_json::from_str(&token_body)
                .with_context(|| {
                    format!("failed to parse Feishu tenant_access_token response: {token_body}")
                })?;
            if token_payload.code != 0 {
                return Err(anyhow!(
                    "Feishu tenant_access_token request failed: {}",
                    token_payload
                        .msg
                        .unwrap_or_else(|| "unknown token error".to_owned())
                ));
            }
            let tenant_access_token = token_payload.tenant_access_token.ok_or_else(|| {
                anyhow!("Feishu tenant_access_token response did not include a token")
            })?;

            let message_response = client
                .post(format!(
                    "https://open.feishu.cn/open-apis/im/v1/messages?receive_id_type={}",
                    receive_id_type.as_api_str()
                ))
                .bearer_auth(tenant_access_token)
                .json(&request_body)
                .send()
                .await
                .context("failed to send Feishu app bot direct message")?;
            let message_status = message_response.status();
            let message_body = message_response
                .text()
                .await
                .context("failed to read Feishu app bot message response body")?;
            if !message_status.is_success() {
                return Err(anyhow!(
                    "Feishu app bot message request returned HTTP {}: {}",
                    message_status,
                    message_body.trim()
                ));
            }
            let message_payload: FeishuAppBotMessageResponse = serde_json::from_str(&message_body)
                .with_context(|| {
                    format!("failed to parse Feishu app bot message response: {message_body}")
                })?;
            if message_payload.code != 0 {
                return Err(anyhow!(
                    "Feishu app bot message send failed: {}",
                    message_payload
                        .msg
                        .unwrap_or_else(|| "unknown send error".to_owned())
                ));
            }

            Ok(())
        })
    }
}

fn build_feishu_client(timeout_secs: u64) -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(timeout_secs.max(1)))
        .build()
        .context("failed to build Feishu HTTP client")
}

fn format_feishu_text(label: &str, notification: &Notification) -> String {
    let mut sections = vec![format!(
        "[{}][{}] {}",
        label,
        notification.level.label(),
        notification.title
    )];

    if !notification.body.trim().is_empty() {
        sections.push(notification.body.trim().to_owned());
    }

    sections.join("\n\n")
}

#[derive(Debug, Serialize)]
struct FeishuTextContent {
    text: String,
}

#[derive(Debug, Serialize)]
struct FeishuAppBotTokenRequest {
    app_id: String,
    app_secret: String,
}

#[derive(Debug, Deserialize)]
struct FeishuAppBotTokenResponse {
    code: i64,
    #[serde(default)]
    msg: Option<String>,
    #[serde(default)]
    tenant_access_token: Option<String>,
}

#[derive(Debug, Serialize)]
struct FeishuAppBotMessageRequest {
    receive_id: String,
    msg_type: &'static str,
    content: String,
}

#[derive(Debug, Deserialize)]
struct FeishuAppBotMessageResponse {
    code: i64,
    #[serde(default)]
    msg: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_bot_text_format_uses_label_and_body() {
        let sink = FeishuAppBotSink::new(
            "cli_test".to_owned(),
            "secret".to_owned(),
            "you@example.com".to_owned(),
            FeishuReceiveIdType::Email,
            "connor-mbp".to_owned(),
            5,
        )
        .expect("sink should build");
        let text = sink.format_text(&Notification::new(
            EventLevel::Info,
            "agent run completed for openai/symphony#42",
            "Summary: fixed CI",
        ));

        assert_eq!(
            text,
            "[connor-mbp][info] agent run completed for openai/symphony#42\n\nSummary: fixed CI"
        );
    }

    #[test]
    fn app_bot_message_request_encodes_text_content_as_json_string() {
        let sink = FeishuAppBotSink::new(
            "cli_test".to_owned(),
            "secret".to_owned(),
            "you@example.com".to_owned(),
            FeishuReceiveIdType::Email,
            "connor-mbp".to_owned(),
            5,
        )
        .expect("sink should build");
        let request = sink
            .message_request_body("hello".to_owned())
            .expect("request should build");

        assert_eq!(request.receive_id, "you@example.com");
        assert_eq!(request.msg_type, "text");
        assert_eq!(request.content, r#"{"text":"hello"}"#);
    }
}
