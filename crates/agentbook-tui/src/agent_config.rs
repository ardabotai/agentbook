use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// How the provider authenticates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthType {
    ApiKey,
    OAuth,
    None,
}

/// Stored OAuth tokens (access + refresh).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthCredentials {
    pub refresh: String,
    pub access: String,
    pub expires: u64,
}

/// Persisted agent inference configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub provider: String,
    pub model: String,
    pub auth_type: AuthType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oauth_credentials: Option<OAuthCredentials>,
}

/// A provider the user can select during setup.
pub struct ProviderOption {
    pub label: &'static str,
    pub provider_id: &'static str,
    pub auth_type: AuthType,
    pub env_var: &'static str,
    pub default_model: &'static str,
}

/// All available provider options in display order.
pub const PROVIDERS: &[ProviderOption] = &[
    ProviderOption {
        label: "Claude Pro/Max (subscription)",
        provider_id: "anthropic",
        auth_type: AuthType::OAuth,
        env_var: "ANTHROPIC_OAUTH_TOKEN",
        default_model: "claude-sonnet-4-5-20250929",
    },
    ProviderOption {
        label: "ChatGPT Plus/Pro (subscription)",
        provider_id: "openai-codex",
        auth_type: AuthType::OAuth,
        env_var: "",
        default_model: "gpt-5.1",
    },
    ProviderOption {
        label: "Anthropic API Key",
        provider_id: "anthropic",
        auth_type: AuthType::ApiKey,
        env_var: "ANTHROPIC_API_KEY",
        default_model: "claude-sonnet-4-5-20250929",
    },
    ProviderOption {
        label: "OpenAI API Key",
        provider_id: "openai",
        auth_type: AuthType::ApiKey,
        env_var: "OPENAI_API_KEY",
        default_model: "gpt-4.1",
    },
    ProviderOption {
        label: "Google Gemini",
        provider_id: "google",
        auth_type: AuthType::ApiKey,
        env_var: "GEMINI_API_KEY",
        default_model: "gemini-2.5-flash",
    },
    ProviderOption {
        label: "OpenRouter",
        provider_id: "openrouter",
        auth_type: AuthType::ApiKey,
        env_var: "OPENROUTER_API_KEY",
        default_model: "anthropic/claude-sonnet-4.5",
    },
    ProviderOption {
        label: "Groq",
        provider_id: "groq",
        auth_type: AuthType::ApiKey,
        env_var: "GROQ_API_KEY",
        default_model: "llama-3.3-70b-versatile",
    },
    ProviderOption {
        label: "Ollama (local)",
        provider_id: "ollama",
        auth_type: AuthType::None,
        env_var: "",
        default_model: "llama3.1",
    },
];

/// Path to the agent config file.
pub fn config_path() -> PathBuf {
    let state_dir = std::env::var("XDG_STATE_HOME").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        format!("{home}/.local/state")
    });
    PathBuf::from(state_dir)
        .join("agentbook")
        .join("agent.json")
}

/// Load the agent config from disk, if it exists.
pub fn load_agent_config() -> Option<AgentConfig> {
    let path = config_path();
    let data = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Save the agent config to disk with 0600 permissions.
pub fn save_agent_config(config: &AgentConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating config dir {}", parent.display()))?;
    }

    let data = serde_json::to_string_pretty(config)?;
    fs::write(&path, &data).with_context(|| format!("writing config to {}", path.display()))?;

    // Set file permissions to 0600 (owner read/write only)
    set_permissions_0600(&path)?;

    Ok(())
}

#[cfg(unix)]
fn set_permissions_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(0o600);
    fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_permissions_0600(_path: &Path) -> Result<()> {
    Ok(())
}

/// Returns the (env_var_name, env_var_value) pair needed to launch the agent.
/// For OAuth, returns the `AGENTBOOK_OAUTH_CREDENTIALS` env var with JSON credentials.
/// For API key, returns the provider's standard env var with the key value.
pub fn provider_env_vars(config: &AgentConfig) -> Vec<(String, String)> {
    let mut vars = vec![(
        "AGENTBOOK_MODEL".to_string(),
        format!("{}:{}", config.provider, config.model),
    )];

    match config.auth_type {
        AuthType::ApiKey => {
            if let Some(ref key) = config.api_key {
                // Find the matching provider to get its env var name
                if let Some(p) = PROVIDERS
                    .iter()
                    .find(|p| p.provider_id == config.provider && p.auth_type == AuthType::ApiKey)
                {
                    vars.push((p.env_var.to_string(), key.clone()));
                }
            }
        }
        AuthType::OAuth => {
            if let Some(ref creds) = config.oauth_credentials
                && let Ok(json) = serde_json::to_string(creds)
            {
                vars.push(("AGENTBOOK_OAUTH_CREDENTIALS".to_string(), json));
            }
        }
        AuthType::None => {}
    }

    vars
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_env_vars_api_key() {
        let config = AgentConfig {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            auth_type: AuthType::ApiKey,
            api_key: Some("sk-test-123".to_string()),
            oauth_credentials: None,
        };
        let vars = provider_env_vars(&config);
        assert_eq!(vars.len(), 2);
        assert_eq!(
            vars[0],
            (
                "AGENTBOOK_MODEL".to_string(),
                "anthropic:claude-sonnet-4-5-20250929".to_string()
            )
        );
        assert_eq!(
            vars[1],
            ("ANTHROPIC_API_KEY".to_string(), "sk-test-123".to_string())
        );
    }

    #[test]
    fn test_provider_env_vars_oauth() {
        let config = AgentConfig {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            auth_type: AuthType::OAuth,
            api_key: None,
            oauth_credentials: Some(OAuthCredentials {
                refresh: "refresh-tok".to_string(),
                access: "access-tok".to_string(),
                expires: 9999999999,
            }),
        };
        let vars = provider_env_vars(&config);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].0, "AGENTBOOK_MODEL");
        assert_eq!(vars[1].0, "AGENTBOOK_OAUTH_CREDENTIALS");
        assert!(vars[1].1.contains("refresh-tok"));
    }

    #[test]
    fn test_provider_env_vars_none() {
        let config = AgentConfig {
            provider: "ollama".to_string(),
            model: "llama3.1".to_string(),
            auth_type: AuthType::None,
            api_key: None,
            oauth_credentials: None,
        };
        let vars = provider_env_vars(&config);
        assert_eq!(vars.len(), 1);
        assert_eq!(vars[0].0, "AGENTBOOK_MODEL");
    }

    #[test]
    fn test_serialization_roundtrip() {
        let config = AgentConfig {
            provider: "anthropic".to_string(),
            model: "claude-sonnet-4-5-20250929".to_string(),
            auth_type: AuthType::OAuth,
            api_key: None,
            oauth_credentials: Some(OAuthCredentials {
                refresh: "r".to_string(),
                access: "a".to_string(),
                expires: 123,
            }),
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: AgentConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.provider, "anthropic");
        assert_eq!(restored.auth_type, AuthType::OAuth);
        assert!(restored.api_key.is_none());
        assert!(restored.oauth_credentials.is_some());
    }
}
