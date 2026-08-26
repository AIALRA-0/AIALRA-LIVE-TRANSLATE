//! DingTalk A1 control and post-recording APIs remain separate from the real-time PCM contract.

use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::{Value, json};
use std::env;

#[derive(Clone)]
pub struct DingtalkClient {
    base_url: String,
    access_token: Option<String>,
    team_code: Option<String>,
    user_id: Option<String>,
    client: Client,
}

impl DingtalkClient {
    pub fn from_env() -> Self {
        Self {
            base_url: env::var("DINGTALK_API_BASE")
                .unwrap_or_else(|_| "https://api.dingtalk.com".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            access_token: nonempty_env("DINGTALK_ACCESS_TOKEN"),
            team_code: nonempty_env("DINGTALK_TEAM_CODE"),
            user_id: nonempty_env("DINGTALK_USER_ID"),
            client: Client::new(),
        }
    }

    pub fn configured(&self) -> bool {
        self.access_token.is_some() && self.team_code.is_some() && self.user_id.is_some()
    }

    pub async fn start_recording(&self, session_id: &str) -> Result<Value> {
        self.control_recording("start", session_id).await
    }

    pub async fn stop_recording(&self, session_id: &str) -> Result<Value> {
        self.control_recording("stop", session_id).await
    }

    async fn control_recording(&self, action: &str, session_id: &str) -> Result<Value> {
        let token = self
            .access_token
            .as_deref()
            .context("DINGTALK_ACCESS_TOKEN is not configured")?;
        let team_code = self
            .team_code
            .as_deref()
            .context("DINGTALK_TEAM_CODE is not configured")?;
        let user_id = self
            .user_id
            .as_deref()
            .context("DINGTALK_USER_ID is not configured")?;
        // outBizData binds the device-side recording to the durable AIALRA session identifier.
        let response = self
            .client
            .post(format!(
                "{}/v1.0/dvi/devices/recording/control",
                self.base_url
            ))
            .header("x-acs-dingtalk-access-token", token)
            .json(&json!({
                "action": action,
                "agree": true,
                "outBizData": json!({"businessOrder": session_id}).to_string(),
                "teamCode": team_code,
                "userId": user_id
            }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(response)
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.trim().is_empty())
}
