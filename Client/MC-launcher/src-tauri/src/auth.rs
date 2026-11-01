use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceCodeInfo {
    flow_id: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Serialize, Clone)]
pub(crate) struct MicrosoftProfile {
    id: String,
    username: String,
    uuid: String,
    #[serde(rename = "account_type")]
    account_type: String,
}

#[derive(Clone)]
struct PendingFlow {
    client_id: String,
    device_code: String,
    expires_at: Instant,
    interval: u64,
}

static PENDING_FLOWS: OnceLock<Mutex<HashMap<String, PendingFlow>>> = OnceLock::new();

fn prune_expired_flows(flows: &mut HashMap<String, PendingFlow>, now: Instant) {
    flows.retain(|_, flow| flow.expires_at > now);
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: Option<u64>,
    message: Option<String>,
}

#[derive(Deserialize)]
struct OAuthToken {
    access_token: String,
    refresh_token: String,
}

#[derive(Deserialize)]
struct OAuthError {
    error: String,
}

#[derive(Deserialize)]
struct XboxResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: XboxDisplayClaims,
}

#[derive(Deserialize)]
struct XboxDisplayClaims {
    xui: Vec<XboxUserClaim>,
}

#[derive(Deserialize)]
struct XboxUserClaim {
    uhs: String,
    xid: Option<String>,
}

#[derive(Deserialize)]
struct MinecraftToken {
    access_token: String,
}

#[derive(Deserialize)]
struct MinecraftProfile {
    id: String,
    name: String,
}

#[derive(Clone)]
pub(crate) struct MinecraftSession {
    pub access_token: String,
    pub xuid: String,
}

fn oauth_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("MyLauncher/0.1.0")
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())
}

fn credential_entry_for(service: &str, account_id: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, account_id).map_err(|error| error.to_string())
}

fn credential_entry(account_id: &str) -> Result<keyring::Entry, String> {
    credential_entry_for("com.mylauncher2.desktop", account_id)
}

fn legacy_credential_entry(account_id: &str) -> Result<keyring::Entry, String> {
    credential_entry_for("com.mylauncher.desktop", account_id)
}

fn refresh_token(account_id: &str) -> Result<String, String> {
    let current_entry = credential_entry(account_id)?;
    match current_entry.get_password() {
        Ok(token) => Ok(token),
        Err(keyring::Error::NoEntry) => {
            let legacy_entry = legacy_credential_entry(account_id)?;
            let token = legacy_entry
                .get_password()
                .map_err(|error| error.to_string())?;
            current_entry
                .set_password(&token)
                .map_err(|error| error.to_string())?;
            let _ = legacy_entry.delete_credential();
            Ok(token)
        }
        Err(error) => Err(error.to_string()),
    }
}

async fn exchange_xbox_tokens(msa_access_token: &str) -> Result<MinecraftSession, String> {
    let client = oauth_client()?;
    let xbox: XboxResponse = client
        .post("https://user.auth.xboxlive.com/user/authenticate")
        .json(&serde_json::json!({
            "Properties": {
                "AuthMethod": "RPS",
                "SiteName": "user.auth.xboxlive.com",
                "RpsTicket": format!("d={msa_access_token}"),
            },
            "RelyingParty": "http://auth.xboxlive.com",
            "TokenType": "JWT",
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    xbox.display_claims
        .xui
        .first()
        .ok_or("Xbox Live не вернул user hash.")?;
    let xsts: XboxResponse = client
        .post("https://xsts.auth.xboxlive.com/xsts/authorize")
        .json(&serde_json::json!({
            "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbox.token] },
            "RelyingParty": "rp://api.minecraftservices.com/",
            "TokenType": "JWT",
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let xsts_hash = xsts
        .display_claims
        .xui
        .first()
        .map(|claim| claim.uhs.as_str())
        .ok_or("Xbox XSTS не вернул user hash.")?;
    let minecraft: MinecraftToken = client
        .post("https://api.minecraftservices.com/authentication/login_with_xbox")
        .json(&serde_json::json!({
            "identityToken": format!("XBL3.0 x={xsts_hash};{}", xsts.token),
        }))
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let xuid = xsts
        .display_claims
        .xui
        .first()
        .and_then(|claim| claim.xid.clone())
        .unwrap_or_default();
    Ok(MinecraftSession {
        access_token: minecraft.access_token,
        xuid,
    })
}

#[tauri::command]
pub(crate) async fn request_microsoft_device_code(
    client_id: String,
) -> Result<DeviceCodeInfo, String> {
    if client_id.len() < 16
        || client_id.len() > 64
        || !client_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("Укажите корректный публичный Microsoft Client ID в настройках.".into());
    }
    let response: DeviceCodeResponse = oauth_client()?
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode")
        .form(&[
            ("client_id", client_id.as_str()),
            ("scope", "XboxLive.signin offline_access"),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let verification_uri =
        reqwest::Url::parse(&response.verification_uri).map_err(|error| error.to_string())?;
    if verification_uri.scheme() != "https"
        || ![
            "microsoft.com",
            "www.microsoft.com",
            "login.microsoftonline.com",
        ]
        .contains(&verification_uri.host_str().unwrap_or_default())
    {
        return Err("Microsoft вернул недоверенную ссылку подтверждения входа.".into());
    }
    let flow_id = uuid::Uuid::new_v4().to_string();
    let interval = response.interval.unwrap_or(5).max(2);
    let expires_in = response.expires_in;
    let expires_at = Instant::now()
        .checked_add(Duration::from_secs(expires_in))
        .ok_or("Microsoft вернул некорректный срок действия кода.")?;
    let _ = response.message;
    let mut flows = PENDING_FLOWS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "Не удалось сохранить состояние Microsoft login.".to_string())?;
    prune_expired_flows(&mut flows, Instant::now());
    flows.insert(
        flow_id.clone(),
        PendingFlow {
            client_id,
            device_code: response.device_code,
            expires_at,
            interval,
        },
    );
    drop(flows);
    let cleanup_after = Duration::from_secs(expires_in);
    tokio::spawn(async move {
        tokio::time::sleep(cleanup_after).await;
        if let Some(flows) = PENDING_FLOWS.get()
            && let Ok(mut flows) = flows.lock()
        {
            prune_expired_flows(&mut flows, Instant::now());
        }
    });
    Ok(DeviceCodeInfo {
        flow_id,
        user_code: response.user_code,
        verification_uri: response.verification_uri,
        expires_in,
        interval,
    })
}

#[tauri::command]
pub(crate) async fn complete_microsoft_login(
    _app: AppHandle,
    flow_id: String,
) -> Result<MicrosoftProfile, String> {
    let flow = PENDING_FLOWS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "Не удалось прочитать состояние Microsoft login.".to_string())?
        .remove(&flow_id)
        .ok_or("Код входа не найден или уже использован.")?;
    let client = oauth_client()?;
    let token_url = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
    let remaining = flow.expires_at.saturating_duration_since(Instant::now());
    let deadline = tokio::time::Instant::now() + remaining;
    let mut interval = flow.interval;
    let token = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err("Срок действия кода Microsoft истёк. Запросите новый код.".into());
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
        let response = client
            .post(token_url)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("client_id", flow.client_id.as_str()),
                ("device_code", flow.device_code.as_str()),
            ])
            .send()
            .await
            .map_err(|error| error.to_string())?;
        if response.status().is_success() {
            break response
                .json::<OAuthToken>()
                .await
                .map_err(|error| error.to_string())?;
        }
        let error = response
            .json::<OAuthError>()
            .await
            .map_err(|error| error.to_string())?;
        match error.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += 5;
                continue;
            }
            "authorization_declined" => {
                return Err("Вход Microsoft был отменён пользователем.".into());
            }
            "expired_token" => {
                return Err("Срок действия кода Microsoft истёк. Запросите новый код.".into());
            }
            _ => return Err(format!("Ошибка Microsoft OAuth: {}", error.error)),
        }
    };
    let session = exchange_xbox_tokens(&token.access_token).await?;
    let client = oauth_client()?;
    let profile = client
        .get("https://api.minecraftservices.com/minecraft/profile")
        .bearer_auth(&session.access_token)
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if profile.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("У этого Microsoft аккаунта нет лицензии Minecraft Java Edition.".into());
    }
    let profile: MinecraftProfile = profile
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    let entry = credential_entry(&profile.id)?;
    entry
        .set_password(&token.refresh_token)
        .map_err(|error| error.to_string())?;
    Ok(MicrosoftProfile {
        id: profile.id.clone(),
        username: profile.name,
        uuid: profile.id,
        account_type: "microsoft".into(),
    })
}

pub(crate) async fn refresh_minecraft_session(
    account_id: &str,
    client_id: &str,
) -> Result<MinecraftSession, String> {
    let refresh_token = refresh_token(account_id)?;
    let token: OAuthToken = oauth_client()?
        .post("https://login.microsoftonline.com/consumers/oauth2/v2.0/token")
        .form(&[
            ("client_id", client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_token),
            ("scope", "XboxLive.signin offline_access"),
        ])
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?
        .json()
        .await
        .map_err(|error| error.to_string())?;
    credential_entry(account_id)?
        .set_password(&token.refresh_token)
        .map_err(|error| error.to_string())?;
    exchange_xbox_tokens(&token.access_token).await
}

#[tauri::command]
pub(crate) fn remove_microsoft_tokens(account_id: String) -> Result<(), String> {
    let mut failures = Vec::new();
    for entry in [credential_entry(&account_id), legacy_credential_entry(&account_id)] {
        match entry {
            Ok(entry) => match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => failures.push(error.to_string()),
            },
            Err(error) => failures.push(error),
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::{MicrosoftProfile, PendingFlow, prune_expired_flows};
    use std::{
        collections::HashMap,
        time::{Duration, Instant},
    };

    #[test]
    fn expired_device_code_flows_are_pruned() {
        let now = Instant::now();
        let mut flows = HashMap::from([
            (
                "expired".into(),
                PendingFlow {
                    client_id: "client".into(),
                    device_code: "expired-code".into(),
                    expires_at: now - Duration::from_secs(1),
                    interval: 5,
                },
            ),
            (
                "active".into(),
                PendingFlow {
                    client_id: "client".into(),
                    device_code: "active-code".into(),
                    expires_at: now + Duration::from_secs(60),
                    interval: 5,
                },
            ),
        ]);

        prune_expired_flows(&mut flows, now);

        assert_eq!(flows.len(), 1);
        assert!(flows.contains_key("active"));
    }

    #[test]
    fn microsoft_profile_uses_the_snake_case_account_type_expected_by_the_frontend() {
        let profile = MicrosoftProfile {
            id: "069a79f444e94726a5befca90e38aaf5".into(),
            username: "Player".into(),
            uuid: "069a79f444e94726a5befca90e38aaf5".into(),
            account_type: "microsoft".into(),
        };
        let value = serde_json::to_value(profile).unwrap();
        assert_eq!(value["account_type"], "microsoft");
        assert!(value.get("accountType").is_none());
    }
}
