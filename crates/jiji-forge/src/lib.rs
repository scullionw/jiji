//! Shared remote workflow engine for PR stacks, landing, and auto-land.
//!
//! This crate should stay headless so the same logic can be hosted by the
//! Tauri app now and a future CLI later.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use ts_rs::TS;
use url::Url;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ForgeProvider {
    GitHub,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct ForgeCapabilities {
    pub provider: ForgeProvider,
    pub supports_pr_stacks: bool,
    pub supports_auto_land: bool,
}

pub struct GitHubForge {
    client: Client,
    api_base: Url,
}

impl GitHubForge {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
            api_base: Url::parse("https://api.github.com")
                .expect("hard-coded GitHub API URL should always parse"),
        }
    }

    pub fn capabilities(&self) -> ForgeCapabilities {
        let _ = &self.client;
        let _ = &self.api_base;

        ForgeCapabilities {
            provider: ForgeProvider::GitHub,
            supports_pr_stacks: true,
            supports_auto_land: true,
        }
    }
}
