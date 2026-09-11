use sha2::{Digest, Sha256};

use crate::AiConfig;

/// Returns a non-secret digest binding a credential source to its provider target.
///
/// The digest deliberately excludes credential values. A trailing slash on the
/// Base URL does not change the target because the network provider resolves
/// both spellings to the same endpoint.
pub fn provider_authorization_digest(config: &AiConfig, credential_source: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"aicoach-provider-authorization-v1\0");
    update_field(&mut hasher, b"provider", &config.provider);
    update_field(
        &mut hasher,
        b"base-url",
        config.base_url.trim().trim_end_matches('/'),
    );
    update_field(&mut hasher, b"api-key-env", &config.api_key_env);
    update_field(&mut hasher, b"credential-source", credential_source);
    format!("sha256:{:x}", hasher.finalize())
}

fn update_field(hasher: &mut Sha256, label: &[u8], value: &str) {
    // Hashing each value separately makes the outer framing unambiguous even
    // if this public helper is called before configuration validation.
    hasher.update(label);
    hasher.update([0]);
    hasher.update(Sha256::digest(value.as_bytes()));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured_ai() -> AiConfig {
        AiConfig {
            provider: "openai-compatible".to_owned(),
            base_url: "https://provider.example/v1".to_owned(),
            models: crate::AiModels {
                completion: "completion-model".to_owned(),
                error_analysis: "analysis-model".to_owned(),
                chat: "chat-model".to_owned(),
            },
            ..AiConfig::default()
        }
    }

    #[test]
    fn trailing_base_url_slashes_have_the_same_authorization_digest() {
        let config = configured_ai();
        let mut slash = config.clone();
        slash.base_url.push_str("///");

        assert_eq!(
            provider_authorization_digest(&config, "keychain"),
            provider_authorization_digest(&slash, "keychain")
        );
    }

    #[test]
    fn digest_format_is_stable() {
        assert_eq!(
            provider_authorization_digest(&configured_ai(), "keychain"),
            "sha256:7f08a663664df33a2600d2f4f3aa609babcb6b3ca71a69f530caa47e43eb1481"
        );
    }

    #[test]
    fn provider_target_key_name_and_credential_source_are_bound() {
        let config = configured_ai();
        let original = provider_authorization_digest(&config, "keychain");

        let mut changed_provider = config.clone();
        changed_provider.provider = "disabled".to_owned();
        assert_ne!(
            original,
            provider_authorization_digest(&changed_provider, "keychain")
        );

        let mut changed_target = config.clone();
        changed_target.base_url = "https://other.example/v1".to_owned();
        assert_ne!(
            original,
            provider_authorization_digest(&changed_target, "keychain")
        );

        let mut changed_key_name = config.clone();
        changed_key_name.api_key_env = "OTHER_API_KEY".to_owned();
        assert_ne!(
            original,
            provider_authorization_digest(&changed_key_name, "keychain")
        );

        assert_ne!(
            original,
            provider_authorization_digest(&config, "environment")
        );
    }
}
