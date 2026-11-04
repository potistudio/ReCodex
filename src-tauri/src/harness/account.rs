//! Model connection selection and the Command Code account.
use super::{
    command_code::{self, PendingLogin, Source, Store},
    model::{ModelClient, OpenAiCompatible},
    Harness, HarnessConfig,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex as StdMutex};

pub const COMMAND_CODE: &str = "Command Code";
const OPENAI_COMPATIBLE: &str = "OpenAI-compatible";

pub(crate) struct Account {
    pub config: HarnessConfig,
    store: Option<Store>,
    studio_url: String,
    api_url: String,
    first_port: u16,
    login: StdMutex<Option<tokio::task::AbortHandle>>,
}

impl Account {
    pub fn new(config: HarnessConfig, store: Store) -> Self {
        Self {
            config,
            store: Some(store),
            studio_url: command_code::STUDIO_URL.into(),
            api_url: command_code::API_URL.into(),
            first_port: command_code::FIRST_PORT,
            login: StdMutex::new(None),
        }
    }

    /// No credential store; used when a test supplies its own model client.
    #[cfg(test)]
    pub fn disabled() -> Self {
        Self {
            config: HarnessConfig::default(),
            store: None,
            studio_url: String::new(),
            api_url: String::new(),
            first_port: 0,
            login: StdMutex::new(None),
        }
    }

    #[cfg(test)]
    pub fn for_test(config: HarnessConfig, store: Store, studio_url: &str, api_url: &str) -> Self {
        Self {
            config,
            store: Some(store),
            studio_url: studio_url.into(),
            api_url: api_url.into(),
            first_port: 0,
            login: StdMutex::new(None),
        }
    }
}

impl Harness {
    /// Choose the model connection: an explicit OpenAI-compatible endpoint, then Command Code
    /// credentials, then `OPENAI_API_KEY`.
    pub fn reconnect(&self) {
        let account = &self.account;
        let config = &account.config;
        let credentials = account.store.as_ref().and_then(Store::load);
        let (client, label): (Result<Arc<dyn ModelClient>, String>, &str) = if config.explicit() {
            (config.openai(config.api_key.clone()), OPENAI_COMPATIBLE)
        } else if let Some((credentials, _)) = credentials {
            (
                OpenAiCompatible::new(
                    format!("{}{}", account.api_url, command_code::PROVIDER_PATH),
                    Some(credentials.api_key),
                    config.models.clone(),
                )
                .map(|client| Arc::new(client) as Arc<dyn ModelClient>),
                COMMAND_CODE,
            )
        } else if let Some(key) = &config.openai_api_key {
            (config.openai(Some(key.clone())), OPENAI_COMPATIBLE)
        } else {
            (
                Err("Sign in to Command Code, or set RECODEX_OPENAI_API_KEY or RECODEX_OPENAI_BASE_URL".into()),
                COMMAND_CODE,
            )
        };
        self.set_client(client, label.into());
    }

    /// The Command Code account state shown in settings. Never includes the key itself.
    pub fn account(&self) -> Value {
        let store = self.account.store.as_ref();
        let pending = self.account.login.lock().unwrap().is_some();
        let mut account = json!({
            "available": store.is_some(),
            "connection": self.connection_label(),
            "loginPending": pending,
            "signedIn": false
        });
        if let Some((credentials, source)) = store.and_then(Store::load) {
            account["signedIn"] = json!(true);
            account["userName"] = json!(credentials.user_name);
            account["keyName"] = json!(credentials.key_name);
            account["source"] = json!(source.label());
            // Only ReCodex's own sign-in can be removed here.
            account["canSignOut"] = json!(source == Source::ReCodex);
        }
        account
    }

    /// Start a browser sign-in and return the URL to open. The result arrives as an
    /// `account/updated` or `account/login/completed` event.
    pub async fn login_start(self: &Arc<Self>) -> Result<String, String> {
        let store = self
            .account
            .store
            .clone()
            .ok_or("Command Code sign-in is not available")?;
        self.login_cancel();
        let pending =
            PendingLogin::start(&self.account.studio_url, self.account.first_port).await?;
        let url = pending.auth_url.clone();
        let harness = self.clone();
        let api_url = self.account.api_url.clone();
        let task = tokio::spawn(async move {
            let result = async {
                let mut credentials = pending.finish(command_code::LOGIN_TIMEOUT).await?;
                let whoami = command_code::validate(&api_url, &credentials.api_key).await?;
                if credentials.user_name.is_empty() {
                    if let Some(name) = whoami["user"]["userName"].as_str() {
                        credentials.user_name = name.to_owned();
                    }
                }
                store.save(&credentials)
            }
            .await;
            *harness.account.login.lock().unwrap() = None;
            match result {
                Ok(()) => {
                    harness.reconnect();
                    harness.emit(json!({"method":"account/updated","params":{"success":true}}));
                }
                Err(error) => harness.emit(json!({
                    "method":"account/login/completed",
                    "params":{"success":false,"error":{"message":error}}
                })),
            }
        });
        *self.account.login.lock().unwrap() = Some(task.abort_handle());
        Ok(url)
    }

    pub fn login_cancel(&self) {
        if let Some(task) = self.account.login.lock().unwrap().take() {
            task.abort();
        }
    }

    pub fn logout(&self) -> Result<Value, String> {
        self.login_cancel();
        self.account
            .store
            .as_ref()
            .ok_or("Command Code sign-in is not available")?
            .clear()?;
        self.reconnect();
        self.emit(json!({"method":"account/updated","params":{"success":true}}));
        Ok(self.account())
    }
}
