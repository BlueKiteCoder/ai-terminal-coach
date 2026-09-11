use super::{
    DaemonAuthorization, DaemonCredentialMode, Paths, authorize_and_refresh_daemon,
    keychain_key_exists, prompt_and_store_keychain_key, write_shell_settings,
};
use aicoach_ai::{AiError, OpenAiCompatibleProvider, PROVIDER_FLIGHT_CHECK_MESSAGE};
use anyhow::{Context, Result, anyhow, bail};
use secrecy::SecretString;
use std::{
    env, fs,
    io::{self, BufRead, IsTerminal, Write},
    path::Path,
    process::{Command, Stdio},
    time::Duration,
};

const FLIGHT_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const FLIGHT_CHECK_RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Language {
    English,
    Chinese,
}

impl Language {
    fn from_config(config: &aicoach_core::Config) -> Self {
        if config.coach.language == "zh-CN" {
            Self::Chinese
        } else {
            Self::English
        }
    }

    fn text<'a>(self, english: &'a str, chinese: &'a str) -> &'a str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }
}

#[derive(Debug)]
struct SetupPlan {
    config: aicoach_core::Config,
    store_key: bool,
    run_flight_check: bool,
    credential_source: Option<CredentialSource>,
    replacement_requires_saved_target: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CredentialSource {
    Keychain,
    Environment,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExistingAuthorization {
    Legacy,
    Bound(CredentialSource),
    ReviewRequired,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CredentialAvailability {
    keychain: bool,
    environment: bool,
}

impl CredentialAvailability {
    fn any(self) -> bool {
        self.keychain || self.environment
    }

    fn preferred(self) -> Option<CredentialSource> {
        if self.keychain {
            Some(CredentialSource::Keychain)
        } else if self.environment {
            Some(CredentialSource::Environment)
        } else {
            None
        }
    }

    fn contains(self, source: CredentialSource) -> bool {
        match source {
            CredentialSource::Keychain => self.keychain,
            CredentialSource::Environment => self.environment,
        }
    }
}

pub(super) fn run(paths: &Paths) -> Result<()> {
    super::ensure_macos()?;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("provider setup is interactive and must run in a terminal");
    }

    let original_config = config_snapshot(&paths.config)?;
    let original_authorization = config_snapshot(&paths.provider_authorization)?;
    let current = load_current_config(paths)?;
    let language = Language::from_config(&current);
    let credentials = credential_availability(&current.ai.api_key_env);
    let authorization = existing_authorization(paths, &current);
    let plan = {
        let stdin = io::stdin();
        let stdout = io::stdout();
        collect_plan_with_authorization(
            &current,
            credentials,
            authorization,
            &mut stdin.lock(),
            &mut stdout.lock(),
        )?
    };

    if plan.run_flight_check && !plan.store_key {
        ensure_authorization_unchanged(
            &paths.provider_authorization,
            original_authorization.as_deref(),
            language,
        )?;
        perform_flight_check(
            &plan.config.ai,
            language,
            plan.credential_source
                .expect("an active provider has a credential source"),
            false,
            plan.replacement_requires_saved_target,
        )?;
    }

    // A requested check with an existing credential happens before this save.
    // A new Keychain credential must be stored afterward; any later check
    // failure reports that the validated config and credential remain saved.
    ensure_config_unchanged(&paths.config, original_config.as_deref(), language)?;
    ensure_authorization_unchanged(
        &paths.provider_authorization,
        original_authorization.as_deref(),
        language,
    )?;
    plan.config.save_to(&paths.config)?;
    let saved_config = config_snapshot(&paths.config)?;
    println!(
        "\x1b[32m✓ {}\x1b[0m",
        language.text("Provider configuration saved", "服务商配置已保存")
    );
    write_shell_settings(paths)?;

    if plan.store_key {
        println!(
            "{}",
            language.text(
                "Enter the API key in the macOS Keychain prompt. It will not be written to config or shell history.",
                "请在 macOS 钥匙串提示框中输入 API Key；它不会写入配置文件或 Shell 历史。"
            )
        );
        prompt_and_store_keychain_key().map_err(|_| {
            anyhow!(language.text(
                "Provider configuration was saved, but macOS Keychain did not save the credential",
                "服务商配置已保存，但 macOS 钥匙串未能保存凭据"
            ))
        })?;
        println!(
            "\x1b[32m✓ {}\x1b[0m",
            language.text(
                "API credential saved in macOS Keychain",
                "API 凭据已保存到 macOS 钥匙串"
            )
        );
        ensure_saved_config_unchanged(&paths.config, saved_config.as_deref(), language, true)?;
    }

    if plan.run_flight_check && plan.store_key {
        perform_flight_check(
            &plan.config.ai,
            language,
            CredentialSource::Keychain,
            true,
            false,
        )?;
    }

    ensure_saved_config_unchanged(
        &paths.config,
        saved_config.as_deref(),
        language,
        plan.store_key,
    )?;
    ensure_authorization_unchanged(
        &paths.provider_authorization,
        original_authorization.as_deref(),
        language,
    )?;
    let daemon_mode = match plan.credential_source {
        Some(CredentialSource::Keychain) => DaemonCredentialMode::Keychain,
        Some(CredentialSource::Environment) => DaemonCredentialMode::Environment,
        None => DaemonCredentialMode::LocalOnly,
    };
    let authorization = DaemonAuthorization::bound(&plan.config.ai, daemon_mode);
    let restarted = authorize_and_refresh_daemon(paths, &authorization).map_err(|error| {
        anyhow!(
            "{}: {error}",
            language.text(
                "provider configuration and any newly stored Keychain credential remain saved, but the daemon was not safely refreshed",
                "服务商配置以及新存入的钥匙串凭据仍已保存，但后台服务未能安全刷新"
            )
        )
    })?;
    let authorization_changed = !matches!(
        super::load_daemon_authorization(paths),
        Ok(Some(current)) if current == authorization
    );
    if config_snapshot(&paths.config)?.as_deref() != saved_config.as_deref()
        || authorization_changed
    {
        if restarted {
            super::stop(paths).context(language.text(
                "configuration or provider authorization changed during provider refresh; the authorization guard prevented use of an unreviewed target, but the daemon could not be stopped",
                "服务商刷新期间配置或服务商授权又被修改；授权保护已阻止使用未经确认的地址，但后台服务无法停止",
            ))?;
            bail!(
                "{}",
                language.text(
                    "configuration or provider authorization changed during provider refresh; the daemon was stopped and its launcher remains bound to the reviewed target—review the current state and rerun `aicoach config setup`",
                    "服务商刷新期间配置或服务商授权又被修改；后台服务已停止，启动器仍绑定到已确认地址——请检查当前状态并重新运行 `aicoach config setup`"
                )
            );
        }
        bail!(
            "{}",
            language.text(
                "configuration or provider authorization changed while the provider launcher was being prepared; no daemon was started and the launcher remains bound to the reviewed target—rerun `aicoach config setup`",
                "准备服务商启动器期间配置或服务商授权又被修改；后台服务没有启动，启动器仍绑定到已确认地址——请重新运行 `aicoach config setup`"
            )
        );
    }

    if plan.credential_source.is_none() {
        println!(
            "\x1b[33m! {}\x1b[0m",
            language.text(
                "The provider remains disabled, so no existing credential can be sent to the new Base URL. Run `aicoach config set-key`, then run `aicoach config setup` again to review and activate it.",
                "服务商仍保持禁用，因此已有凭据不会发送到新的 Base URL。请先运行 `aicoach config set-key`，再运行 `aicoach config setup` 复核并启用。"
            )
        );
        if restarted {
            println!(
                "{}",
                language.text(
                    "The running daemon was refreshed in local-only mode.",
                    "运行中的后台服务已刷新为仅本地模式。"
                )
            );
        }
    } else if plan.credential_source == Some(CredentialSource::Environment) {
        if restarted {
            println!(
                "{}",
                language.text(
                    "The daemon was refreshed with this shell's temporary credential. Store it with `aicoach config set-key` before the next login.",
                    "后台服务已使用当前 Shell 的临时凭据刷新。下次登录前，请运行 `aicoach config set-key` 持久保存。"
                )
            );
        } else {
            println!(
                "{}",
                language.text(
                    "Run `aicoach start` in this shell to inherit its temporary credential. Use `aicoach config set-key` for future logins.",
                    "请在当前 Shell 中运行 `aicoach start` 以继承临时凭据；如需跨登录使用，请运行 `aicoach config set-key`。"
                )
            );
        }
    } else if restarted {
        println!(
            "{}",
            language.text(
                "The running daemon was refreshed. Type a question and press Option+/ to test it.",
                "运行中的后台服务已刷新。输入问题后按 Option+/ 即可测试。"
            )
        );
    } else {
        println!(
            "{}",
            language.text(
                "Start the daemon with `aicoach start`, then type a question and press Option+/.",
                "请先运行 `aicoach start`，然后输入问题并按 Option+/。"
            )
        );
    }
    Ok(())
}

fn load_current_config(paths: &Paths) -> Result<aicoach_core::Config> {
    if paths.config.exists() {
        aicoach_core::Config::load_from(&paths.config)
            .with_context(|| format!("load {}", paths.config.display()))
    } else {
        Ok(aicoach_core::Config::default())
    }
}

fn config_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn existing_authorization(paths: &Paths, current: &aicoach_core::Config) -> ExistingAuthorization {
    match super::load_daemon_authorization(paths) {
        Ok(None) => ExistingAuthorization::Legacy,
        Ok(Some(authorization))
            if super::authorization_matches_config(&authorization, &current.ai) =>
        {
            match authorization.mode {
                DaemonCredentialMode::Keychain => {
                    ExistingAuthorization::Bound(CredentialSource::Keychain)
                }
                DaemonCredentialMode::Environment => {
                    ExistingAuthorization::Bound(CredentialSource::Environment)
                }
                DaemonCredentialMode::LocalOnly => ExistingAuthorization::ReviewRequired,
            }
        }
        Ok(Some(_)) | Err(_) => ExistingAuthorization::ReviewRequired,
    }
}

fn ensure_config_unchanged(path: &Path, original: Option<&[u8]>, language: Language) -> Result<()> {
    let current = config_snapshot(path)?;
    if current.as_deref() != original {
        bail!(
            "{}",
            language.text(
                "configuration changed while setup was open; nothing was saved—review the new values and rerun `aicoach config setup`",
                "设置期间配置已被其他操作修改；本次没有保存，请检查新内容后重新运行 `aicoach config setup`"
            )
        );
    }
    Ok(())
}

fn ensure_authorization_unchanged(
    path: &Path,
    original: Option<&[u8]>,
    language: Language,
) -> Result<()> {
    let current = config_snapshot(path)?;
    if current.as_deref() != original {
        bail!(
            "{}",
            language.text(
                "provider authorization changed while setup was open; nothing was saved—review the current state and rerun `aicoach config setup`",
                "设置期间服务商授权被其他操作修改；本次没有保存——请检查当前状态并重新运行 `aicoach config setup`"
            )
        );
    }
    Ok(())
}

fn ensure_saved_config_unchanged(
    path: &Path,
    saved: Option<&[u8]>,
    language: Language,
    key_was_saved: bool,
) -> Result<()> {
    let current = config_snapshot(path)?;
    if current.as_deref() != saved {
        bail!(
            "{}",
            if key_was_saved {
                language.text(
                    "configuration changed after setup saved it; the new Keychain credential remains saved, but the daemon was not refreshed—review the current config and rerun `aicoach config setup`",
                    "配置在设置保存后又被其他操作修改；新的钥匙串凭据仍已保存，但后台服务未刷新——请检查当前配置并重新运行 `aicoach config setup`"
                )
            } else {
                language.text(
                    "configuration changed after setup saved it; the daemon was not refreshed—review the current config and rerun `aicoach config setup`",
                    "配置在设置保存后又被其他操作修改；后台服务未刷新——请检查当前配置并重新运行 `aicoach config setup`"
                )
            }
        );
    }
    Ok(())
}

#[cfg(test)]
fn collect_plan<R: BufRead, W: Write>(
    current: &aicoach_core::Config,
    credentials: CredentialAvailability,
    input: &mut R,
    output: &mut W,
) -> Result<SetupPlan> {
    collect_plan_with_authorization(
        current,
        credentials,
        ExistingAuthorization::Legacy,
        input,
        output,
    )
}

fn collect_plan_with_authorization<R: BufRead, W: Write>(
    current: &aicoach_core::Config,
    credentials: CredentialAvailability,
    authorization: ExistingAuthorization,
    input: &mut R,
    output: &mut W,
) -> Result<SetupPlan> {
    let language = Language::from_config(current);
    writeln!(
        output,
        "\n\x1b[1m{}\x1b[0m",
        language.text("AI provider setup", "AI 服务商设置")
    )?;
    writeln!(
        output,
        "{}",
        language.text(
            "No endpoint, model, or credential is built in. New provider values are locally validated before they are saved; every optional network check is disclosed first.",
            "项目不内置地址、模型或凭据；新的服务商配置会在保存前完成本地验证，所有可选网络检查都会事先说明。"
        )
    )?;

    let base_url = prompt_base_url(current, input, output, language)?;
    let target_changed =
        normalized_provider_target(&current.ai.base_url) != normalized_provider_target(&base_url);
    let candidate_source = match authorization {
        ExistingAuthorization::Bound(source) if credentials.contains(source) => Some(source),
        ExistingAuthorization::Legacy
        | ExistingAuthorization::Bound(_)
        | ExistingAuthorization::ReviewRequired => credentials.preferred(),
    };
    let credential_confirmation_required = candidate_source.is_some()
        && match authorization {
            ExistingAuthorization::Bound(source) => {
                target_changed || candidate_source != Some(source)
            }
            ExistingAuthorization::Legacy => {
                current.ai.provider != "openai-compatible" || target_changed
            }
            ExistingAuthorization::ReviewRequired => true,
        };
    let existing_credential_confirmed = if credential_confirmation_required {
        let (source, question) = match candidate_source.expect("a credential source is available") {
            CredentialSource::Keychain => (
                language.text("macOS Keychain", "macOS 钥匙串"),
                language.text(
                    "Use the existing macOS Keychain credential with this Base URL?",
                    "允许将 macOS 钥匙串中的现有凭据用于这个 Base URL？",
                ),
            ),
            CredentialSource::Environment => (
                language.text("this shell", "当前 Shell"),
                language.text(
                    "Use this shell's existing credential with this Base URL?",
                    "允许将当前 Shell 中的现有凭据用于这个 Base URL？",
                ),
            ),
        };
        writeln!(
            output,
            "{}",
            language.text(
                "Credentials are scoped to a provider target. Declining keeps the provider disabled and does not send the credential; save a replacement key, then rerun setup to activate it.",
                "凭据应限定于服务商地址。选择否会保持服务商禁用且不会发送凭据；保存替换密钥后，请重新运行设置来启用。"
            )
        )?;
        writeln!(
            output,
            "{}: {source}",
            language.text("Credential source", "凭据来源")
        )?;
        prompt_yes_no(input, output, question, false, language)?
    } else {
        candidate_source.is_some()
    };
    let models = &current.ai.models;
    let models_are_equal = !models.completion.trim().is_empty()
        && models.completion == models.error_analysis
        && models.completion == models.chat;
    let models_are_distinct = !models.completion.trim().is_empty()
        && !models.error_analysis.trim().is_empty()
        && !models.chat.trim().is_empty()
        && !models_are_equal;
    let one_model = prompt_yes_no(
        input,
        output,
        language.text(
            "Use one model for completion, analysis, and chat?",
            "补全、分析和聊天使用同一个模型？",
        ),
        !models_are_distinct,
        language,
    )?;

    let (completion, error_analysis, chat) = if one_model {
        let default = if models_are_equal {
            models.completion.as_str()
        } else {
            ""
        };
        let model = prompt_value(
            input,
            output,
            language.text("Model ID", "模型 ID"),
            default,
            language,
        )?;
        (model.clone(), model.clone(), model)
    } else {
        (
            prompt_value(
                input,
                output,
                language.text("Completion model", "补全模型"),
                &models.completion,
                language,
            )?,
            prompt_value(
                input,
                output,
                language.text("Analysis model", "分析模型"),
                &models.error_analysis,
                language,
            )?,
            prompt_value(
                input,
                output,
                language.text("Chat model", "聊天模型"),
                &models.chat,
                language,
            )?,
        )
    };

    let mut credential_source = if existing_credential_confirmed {
        candidate_source
    } else {
        None
    };
    let mut store_key = false;
    if credentials.any() && !existing_credential_confirmed {
        writeln!(
            output,
            "{}",
            language.text(
                "The existing credential will not be activated for this Base URL. Finish this setup, run `aicoach config set-key`, then rerun setup.",
                "现有凭据不会在这个 Base URL 上启用。请先完成本次设置，再运行 `aicoach config set-key`，然后重新运行设置。"
            )
        )?;
    } else {
        match credential_source {
            Some(CredentialSource::Keychain) => {
                writeln!(
                    output,
                    "{}",
                    language.text(
                        "The authorized macOS Keychain credential will be used. To replace it later, run `aicoach config set-key`.",
                        "将使用已授权的 macOS 钥匙串凭据；如需稍后替换，请运行 `aicoach config set-key`。"
                    )
                )?;
            }
            Some(CredentialSource::Environment) if credentials.keychain => {
                let keep_environment = prompt_yes_no(
                    input,
                    output,
                    language.text(
                        "Keep using the authorized credential from this shell? Choose No only to switch to the existing macOS Keychain credential.",
                        "继续使用已授权的当前 Shell 凭据？只有要改用现有 macOS 钥匙串凭据时才选择否。",
                    ),
                    true,
                    language,
                )?;
                if !keep_environment {
                    credential_source = Some(CredentialSource::Keychain);
                }
            }
            Some(CredentialSource::Environment) => {
                store_key = prompt_yes_no(
                    input,
                    output,
                    language.text(
                        "A credential is available in this shell. Store an API key in macOS Keychain for background launches?",
                        "当前 Shell 中已有凭据。是否将 API Key 存入 macOS 钥匙串，供后台服务启动时使用？",
                    ),
                    true,
                    language,
                )?;
                if store_key {
                    credential_source = Some(CredentialSource::Keychain);
                }
            }
            None => {
                store_key = prompt_yes_no(
                    input,
                    output,
                    language.text(
                        "Store an API key in macOS Keychain now?",
                        "现在将 API Key 存入 macOS 钥匙串？",
                    ),
                    true,
                    language,
                )?;
                if store_key {
                    credential_source = Some(CredentialSource::Keychain);
                }
            }
        }
    }
    let activate_provider = credential_source.is_some();

    let run_flight_check = if activate_provider {
        writeln!(
            output,
            "{} \"{}\" {}",
            language.text(
                "The optional connection check sends one fixed message:",
                "可选连通测试会发送一条固定消息："
            ),
            PROVIDER_FLIGHT_CHECK_MESSAGE,
            language.text(
                "It checks only the chat model and, alongside ordinary protocol fields, sends no cwd, command, history, output, or environment.",
                "它只检查聊天模型；除常规协议字段外，不会发送目录、命令、历史、输出或环境信息。"
            )
        )?;
        writeln!(
            output,
            "{}",
            if store_key {
                language.text(
                    "The locally validated config and new Keychain credential are saved before this check; if it fails, both remain saved and the daemon is not refreshed.",
                    "本地验证通过的配置和新钥匙串凭据会先保存再检查；若失败，二者都会保留，后台服务不会刷新。"
                )
            } else {
                language.text(
                    "This check runs before saving; if it fails, the previous config remains unchanged.",
                    "该检查会在保存前执行；若失败，原配置保持不变。"
                )
            }
        )?;
        prompt_yes_no(
            input,
            output,
            language.text("Run this connection check now?", "现在执行该连通测试？"),
            false,
            language,
        )?
    } else {
        writeln!(
            output,
            "\x1b[33m! {}\x1b[0m",
            if credentials.any() {
                language.text(
                    "No existing credential was authorized for this Base URL, so the connection check will be skipped.",
                    "没有授权将现有凭据用于这个 Base URL，因此将跳过连通测试。"
                )
            } else {
                language.text(
                    "No credential is available, so the connection check will be skipped.",
                    "当前没有可用凭据，因此将跳过连通测试。",
                )
            }
        )?;
        false
    };

    let mut config = current.clone();
    if activate_provider {
        "openai-compatible".clone_into(&mut config.ai.provider);
    } else {
        "disabled".clone_into(&mut config.ai.provider);
    }
    config.ai.base_url = base_url;
    config.ai.models.completion = completion;
    config.ai.models.error_analysis = error_analysis;
    config.ai.models.chat = chat;
    config.validate()?;

    Ok(SetupPlan {
        config,
        store_key,
        run_flight_check,
        credential_source,
        replacement_requires_saved_target: current.ai.provider == "openai-compatible"
            && target_changed,
    })
}

fn normalized_provider_target(value: &str) -> &str {
    value.trim().trim_end_matches('/')
}

fn prompt_base_url<R: BufRead, W: Write>(
    current: &aicoach_core::Config,
    input: &mut R,
    output: &mut W,
    language: Language,
) -> Result<String> {
    loop {
        let value = prompt_value(
            input,
            output,
            language.text("Base URL", "Base URL"),
            &current.ai.base_url,
            language,
        )?;
        let mut candidate = current.clone();
        candidate.ai.base_url.clone_from(&value);
        if candidate.validate().is_ok() {
            return Ok(value);
        }
        writeln!(
            output,
            "{}",
            language.text(
                "Enter an HTTPS URL, or HTTP only for localhost/loopback, without credentials, query, or fragment.",
                "请输入 HTTPS 地址；只有 localhost/回环地址可使用 HTTP，且不得包含凭据、查询参数或片段。"
            )
        )?;
    }
}

fn prompt_value<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    label: &str,
    default: &str,
    language: Language,
) -> Result<String> {
    loop {
        if default.trim().is_empty() {
            write!(output, "{label}: ")?;
        } else {
            write!(output, "{label} [{default}]: ")?;
        }
        output.flush()?;
        let mut value = String::new();
        if input.read_line(&mut value)? == 0 {
            bail!(
                "{}",
                language.text(
                    "provider setup cancelled before completion; configuration was not changed",
                    "服务商设置未完成，配置没有改变"
                )
            );
        }
        let value = value.trim();
        if value.is_empty() && default.trim().is_empty() {
            writeln!(
                output,
                "{} {label}.",
                language.text("A value is required for", "必须填写")
            )?;
            continue;
        }
        return if value.is_empty() {
            Ok(default.trim().to_owned())
        } else {
            Ok(value.to_owned())
        };
    }
}

fn prompt_yes_no<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    prompt: &str,
    default: bool,
    language: Language,
) -> Result<bool> {
    loop {
        write!(
            output,
            "{prompt} {} ",
            if default { "[Y/n]" } else { "[y/N]" }
        )?;
        output.flush()?;
        let mut value = String::new();
        if input.read_line(&mut value)? == 0 {
            bail!(
                "{}",
                language.text(
                    "provider setup cancelled before completion; configuration was not changed",
                    "服务商设置未完成，配置没有改变"
                )
            );
        }
        match value.trim().to_ascii_lowercase().as_str() {
            "" => return Ok(default),
            "y" | "yes" | "是" => return Ok(true),
            "n" | "no" | "否" => return Ok(false),
            _ => writeln!(
                output,
                "{}",
                language.text("Please answer y or n.", "请输入 y 或 n。")
            )?,
        }
    }
}

fn credential_availability(api_key_env: &str) -> CredentialAvailability {
    CredentialAvailability {
        keychain: keychain_key_exists(),
        environment: env::var_os(api_key_env).is_some_and(|value| !value.is_empty()),
    }
}

fn load_credential(
    api_key_env: &str,
    language: Language,
    source: CredentialSource,
) -> Result<SecretString> {
    if source == CredentialSource::Keychain {
        let output = Command::new("/usr/bin/security")
            .args([
                "find-generic-password",
                "-a",
                "AI_COACH_API_KEY",
                "-s",
                "com.aicoach.api-key",
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|_| {
                anyhow!(language.text(
                    "could not read the API credential from macOS Keychain",
                    "无法从 macOS 钥匙串读取 API 凭据"
                ))
            })?;
        if !output.status.success() {
            bail!(
                "{}",
                language.text(
                    "macOS Keychain did not return the configured API credential",
                    "macOS 钥匙串未返回已配置的 API 凭据"
                )
            );
        }
        let mut bytes = output.stdout;
        while matches!(bytes.last(), Some(b'\n' | b'\r')) {
            bytes.pop();
        }
        let value = String::from_utf8(bytes).map_err(|_| {
            anyhow!(language.text(
                "macOS Keychain credential is not valid UTF-8",
                "macOS 钥匙串中的凭据不是有效的 UTF-8 文本"
            ))
        })?;
        if value.trim().is_empty() {
            bail!(
                "{}",
                language.text(
                    "macOS Keychain returned an empty API credential",
                    "macOS 钥匙串返回了空的 API 凭据"
                )
            );
        }
        return Ok(SecretString::from(value));
    }

    let value = env::var(api_key_env).map_err(|_| {
        anyhow!(language.text(
            "API credential is unavailable; store it with `aicoach config set-key`",
            "API 凭据不可用；请运行 `aicoach config set-key` 保存"
        ))
    })?;
    if value.trim().is_empty() {
        bail!(
            "{}",
            language.text(
                "API credential is empty; store it with `aicoach config set-key`",
                "API 凭据为空；请运行 `aicoach config set-key` 保存"
            )
        );
    }
    Ok(SecretString::from(value))
}

fn perform_flight_check(
    config: &aicoach_core::AiConfig,
    language: Language,
    source: CredentialSource,
    setup_already_saved: bool,
    replacement_requires_saved_target: bool,
) -> Result<()> {
    println!(
        "{}",
        language.text(
            "Checking the provider once with the fixed, zero-terminal-context message…",
            "正在使用固定且不含终端上下文的消息检查一次服务商……"
        )
    );
    let credential = load_credential(&config.api_key_env, language, source).map_err(|error| {
        anyhow!(flight_check_start_error(
            &error.to_string(),
            language,
            setup_already_saved
        ))
    })?;
    run_flight_check(config, credential).map_err(|error| {
        anyhow!(flight_check_error(
            &error,
            language,
            setup_already_saved,
            replacement_requires_saved_target
        ))
    })?;
    println!(
        "\x1b[32m✓ {}\x1b[0m",
        language.text(
            "Provider accepted the chat model and credential",
            "服务商已接受聊天模型和凭据"
        )
    );
    Ok(())
}

fn run_flight_check(
    config: &aicoach_core::AiConfig,
    credential: SecretString,
) -> Result<(), AiError> {
    let provider = OpenAiCompatibleProvider::from_api_key(flight_check_config(config), credential)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| AiError::Offline)?;
    let result = runtime.block_on(provider.flight_check());
    runtime.shutdown_timeout(FLIGHT_CHECK_RUNTIME_SHUTDOWN_TIMEOUT);
    result
}

fn flight_check_config(config: &aicoach_core::AiConfig) -> aicoach_ai::OpenAiConfig {
    let mut provider = aicoach_ai::OpenAiConfig::from(config);
    provider.timeouts.chat = provider.timeouts.chat.min(FLIGHT_CHECK_TIMEOUT);
    provider.retry.max_retries = 0;
    provider
}

fn flight_check_start_error(detail: &str, language: Language, setup_already_saved: bool) -> String {
    format!(
        "{}: {detail}; {}",
        language.text("the connection check could not start", "无法开始连通测试"),
        flight_check_state(language, setup_already_saved)
    )
}

fn flight_check_error(
    error: &AiError,
    language: Language,
    setup_already_saved: bool,
    replacement_requires_saved_target: bool,
) -> String {
    let detail = match error {
        AiError::HttpStatus {
            status: 401 | 403, ..
        } if !setup_already_saved && replacement_requires_saved_target => language.text(
            "the existing credential was rejected; rerun setup and decline that credential so the new Base URL is saved disabled, then run `aicoach config set-key` and setup again",
            "现有凭据被拒绝；请重新运行设置并拒绝使用该凭据，让新的 Base URL 以禁用状态保存；随后运行 `aicoach config set-key`，再重新设置",
        ),
        AiError::HttpStatus {
            status: 401 | 403, ..
        } => language.text(
            "the credential was rejected; update it with `aicoach config set-key`",
            "凭据被拒绝；请运行 `aicoach config set-key` 更新",
        ),
        AiError::HttpStatus { status: 300..=399, .. } => language.text(
            "the provider redirected the request; redirects are blocked by the privacy policy, so enter the final HTTPS Base URL",
            "服务商返回了重定向；隐私策略会阻止跟随重定向，请填写最终的 HTTPS Base URL",
        ),
        AiError::HttpStatus { status: 404, .. } => language.text(
            "the endpoint or model was not found; check the Base URL and model ID",
            "未找到接口或模型；请检查 Base URL 和模型 ID",
        ),
        AiError::HttpStatus { status: 429, .. } => language.text(
            "the provider rate-limited the check; wait or verify the account quota",
            "服务商限制了本次请求；请稍后重试或检查账号额度",
        ),
        AiError::HttpStatus {
            status: 408 | 425 | 500..=599,
            ..
        } => language.text(
            "the provider is temporarily unavailable; try the connection check again later",
            "服务商暂时不可用；请稍后重新运行连通测试",
        ),
        AiError::Timeout { .. } => language.text(
            "the check timed out; verify the network and provider availability",
            "连通测试超时；请检查网络和服务商状态",
        ),
        AiError::Transport { .. } | AiError::Offline => language.text(
            "the provider could not be reached; verify the Base URL and network",
            "无法连接服务商；请检查 Base URL 和网络",
        ),
        AiError::InvalidResponse { .. } => language.text(
            "the endpoint responded but was not OpenAI chat-completions compatible",
            "接口已有响应，但不兼容 OpenAI Chat Completions",
        ),
        _ => language.text(
            "review the Base URL, model, and credential",
            "请检查 Base URL、模型和凭据",
        ),
    };
    format!(
        "{}: {detail}; {}",
        language.text("the connection check failed", "连通测试失败"),
        flight_check_state(language, setup_already_saved)
    )
}

fn flight_check_state(language: Language, setup_already_saved: bool) -> &'static str {
    if setup_already_saved {
        language.text(
            "the new config and Keychain credential remain saved, and the running daemon was not refreshed",
            "新配置和钥匙串凭据均已保留，运行中的后台服务尚未刷新",
        )
    } else {
        language.text(
            "provider setup was not saved and the previous config is unchanged",
            "服务商配置未保存，原配置保持不变",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Cursor, Read as _},
        net::TcpListener,
        thread,
        time::Instant,
    };

    #[test]
    fn missing_config_is_loaded_in_memory_without_creating_a_file() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::from_home(directory.path());

        let config = load_current_config(&paths).unwrap();

        assert_eq!(config, aicoach_core::Config::default());
        assert!(!paths.config.exists());
    }

    #[test]
    fn setup_refuses_to_overwrite_a_config_changed_while_prompts_were_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let missing = config_snapshot(&path).unwrap();
        fs::write(&path, b"created elsewhere").unwrap();
        assert!(ensure_config_unchanged(&path, missing.as_deref(), Language::English).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"created elsewhere");

        let original = config_snapshot(&path).unwrap();
        fs::write(&path, b"changed elsewhere").unwrap();
        let error = ensure_config_unchanged(&path, original.as_deref(), Language::English)
            .expect_err("concurrent edit must abort setup");
        assert!(error.to_string().contains("nothing was saved"));
        assert_eq!(fs::read(&path).unwrap(), b"changed elsewhere");

        let saved = config_snapshot(&path).unwrap();
        fs::write(&path, b"changed after save").unwrap();
        let error = ensure_saved_config_unchanged(&path, saved.as_deref(), Language::English, true)
            .expect_err("post-save edit must prevent a daemon refresh");
        assert!(
            error
                .to_string()
                .contains("Keychain credential remains saved")
        );
        assert!(error.to_string().contains("daemon was not refreshed"));

        let authorization_path = directory.path().join("provider-authorization.toml");
        let missing_authorization = config_snapshot(&authorization_path).unwrap();
        fs::write(&authorization_path, b"changed elsewhere").unwrap();
        let error = ensure_authorization_unchanged(
            &authorization_path,
            missing_authorization.as_deref(),
            Language::English,
        )
        .expect_err("concurrent authorization edit must abort setup");
        assert!(error.to_string().contains("authorization changed"));
    }

    #[test]
    fn pristine_setup_without_a_credential_stays_disabled_when_key_storage_is_declined() {
        let current = aicoach_core::Config::default();
        let mut input = Cursor::new(b"https://provider.example/v1\n\nmodel-x\nn\n".as_slice());
        let mut output = Vec::new();
        let plan = collect_plan(
            &current,
            CredentialAvailability::default(),
            &mut input,
            &mut output,
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "disabled");
        assert_eq!(plan.config.ai.base_url, "https://provider.example/v1");
        assert_eq!(plan.config.ai.models.completion, "model-x");
        assert_eq!(plan.config.ai.models.error_analysis, "model-x");
        assert_eq!(plan.config.ai.models.chat, "model-x");
        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
        assert!(plan.credential_source.is_none());
        assert_eq!(plan.credential_source, None);
    }

    #[test]
    fn a_new_key_is_bound_to_keychain_before_provider_activation() {
        let current = aicoach_core::Config::default();
        let mut input = Cursor::new(b"https://provider.example/v1\n\nmodel-x\n\n\n".as_slice());
        let plan = collect_plan(
            &current,
            CredentialAvailability::default(),
            &mut input,
            &mut Vec::new(),
        )
        .unwrap();

        assert!(plan.store_key);
        assert!(plan.credential_source.is_some());
        assert_eq!(plan.credential_source, Some(CredentialSource::Keychain));
    }

    #[test]
    fn disabled_provider_with_a_keychain_credential_stays_disabled_on_default_decline() {
        let current = aicoach_core::Config::default();
        let mut input = Cursor::new(b"https://provider.example/v1\n\n\nmodel-x\n".as_slice());
        let mut output = Vec::new();

        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            &mut input,
            &mut output,
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "disabled");
        assert_eq!(plan.config.ai.base_url, "https://provider.example/v1");
        assert!(plan.credential_source.is_none());
        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Use the existing macOS Keychain credential")
        );
    }

    #[test]
    fn changing_an_active_provider_target_disables_it_when_existing_key_is_declined() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://a.example/v1".to_owned();
        current.ai.models.completion = "model-a".to_owned();
        current.ai.models.error_analysis = "model-a".to_owned();
        current.ai.models.chat = "model-a".to_owned();
        let mut input = Cursor::new(b"https://b.example/v1\nn\n\n\n".as_slice());

        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            &mut input,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "disabled");
        assert_eq!(plan.config.ai.base_url, "https://b.example/v1");
        assert!(plan.credential_source.is_none());
        assert!(!plan.run_flight_check);
    }

    #[test]
    fn changing_an_active_provider_target_activates_it_after_explicit_confirmation() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://a.example/v1".to_owned();
        current.ai.models.completion = "model-a".to_owned();
        current.ai.models.error_analysis = "model-a".to_owned();
        current.ai.models.chat = "model-a".to_owned();
        let mut input = Cursor::new(b"https://b.example/v1\ny\n\n\n\n".as_slice());

        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            &mut input,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "openai-compatible");
        assert_eq!(plan.config.ai.base_url, "https://b.example/v1");
        assert!(plan.credential_source.is_some());
        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
        assert_eq!(plan.credential_source, Some(CredentialSource::Keychain));
    }

    #[test]
    fn an_equivalent_trailing_slash_change_does_not_request_credential_confirmation() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://provider.example/v1/".to_owned();
        current.ai.models.completion = "model-a".to_owned();
        current.ai.models.error_analysis = "model-a".to_owned();
        current.ai.models.chat = "model-a".to_owned();
        let mut input = Cursor::new(b"https://provider.example/v1\n\n\n\n".as_slice());
        let mut output = Vec::new();

        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            &mut input,
            &mut output,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert_eq!(plan.config.ai.provider, "openai-compatible");
        assert_eq!(plan.config.ai.base_url, "https://provider.example/v1");
        assert!(plan.credential_source.is_some());
        assert!(!output.contains("Use the existing macOS Keychain credential"));
    }

    #[test]
    fn existing_distinct_models_are_preserved_by_default() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://provider.example/v1".to_owned();
        current.ai.models.completion = "fast".to_owned();
        current.ai.models.error_analysis = "analysis".to_owned();
        current.ai.models.chat = "smart".to_owned();
        let mut input = Cursor::new(b"\n\n\n\n\n\nn\n".as_slice());
        let mut output = Vec::new();
        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            &mut input,
            &mut output,
        )
        .unwrap();

        assert_eq!(plan.config.ai, current.ai);
        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("config set-key")
        );
    }

    #[test]
    fn invalid_candidate_and_eof_never_mutate_the_source_config() {
        let current = aicoach_core::Config::default();
        let original = current.clone();
        let mut invalid = Cursor::new(b"not-a-url\n".as_slice());
        assert!(
            collect_plan(
                &current,
                CredentialAvailability::default(),
                &mut invalid,
                &mut Vec::new()
            )
            .is_err()
        );
        assert_eq!(current, original);

        assert!(
            collect_plan(
                &current,
                CredentialAvailability::default(),
                &mut Cursor::new(Vec::<u8>::new()),
                &mut Vec::new(),
            )
            .is_err()
        );
        assert_eq!(current, original);
    }

    #[test]
    fn unsafe_remote_http_base_url_is_reprompted_inline() {
        let current = aicoach_core::Config::default();
        let mut input = Cursor::new(
            b"http://provider.example/v1\nhttps://provider.example/v1\n\nmodel-x\nn\n".as_slice(),
        );
        let mut output = Vec::new();

        let plan = collect_plan(
            &current,
            CredentialAvailability::default(),
            &mut input,
            &mut output,
        )
        .unwrap();

        assert_eq!(plan.config.ai.base_url, "https://provider.example/v1");
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("Enter an HTTPS URL")
        );
    }

    #[test]
    fn flight_check_errors_give_one_action_without_echoing_provider_data() {
        let message = flight_check_error(
            &AiError::HttpStatus {
                operation: aicoach_ai::AiOperation::Chat,
                status: 401,
            },
            Language::English,
            false,
            false,
        );
        assert!(message.contains("config set-key"));
        assert!(message.contains("previous config is unchanged"));
        assert!(!message.contains("401"));
    }

    #[test]
    fn saved_key_failure_reports_both_committed_states() {
        let message = flight_check_error(
            &AiError::Timeout {
                operation: aicoach_ai::AiOperation::Chat,
            },
            Language::English,
            true,
            false,
        );
        assert!(message.contains("config and Keychain credential remain saved"));
        assert!(message.contains("daemon was not refreshed"));
    }

    #[test]
    fn changed_target_credential_rejection_guides_saving_disabled_before_replacing_key() {
        let message = flight_check_error(
            &AiError::HttpStatus {
                operation: aicoach_ai::AiOperation::Chat,
                status: 401,
            },
            Language::English,
            false,
            true,
        );
        let save_disabled = message.find("saved disabled").unwrap();
        let replace_key = message.find("config set-key").unwrap();

        assert!(message.contains("decline that credential"));
        assert!(save_disabled < replace_key);
        assert!(message.contains("previous config is unchanged"));
        assert!(!message.contains("401"));
    }

    #[test]
    fn environment_credential_is_not_described_as_a_keychain_item() {
        let current = aicoach_core::Config::default();
        let mut input = Cursor::new(b"https://provider.example/v1\ny\n\nmodel-x\nn\n\n".as_slice());
        let mut output = Vec::new();
        let plan = collect_plan(
            &current,
            CredentialAvailability {
                keychain: false,
                environment: true,
            },
            &mut input,
            &mut output,
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
        assert_eq!(plan.credential_source, Some(CredentialSource::Environment));
        assert!(output.contains("available in this shell"));
        assert!(!output.contains("Replace the existing credential"));
    }

    #[test]
    fn changed_or_unreviewed_authorization_requires_fresh_target_confirmation() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://provider-b.example/v1".to_owned();
        current.ai.models.completion = "model-b".to_owned();
        current.ai.models.error_analysis = "model-b".to_owned();
        current.ai.models.chat = "model-b".to_owned();
        let mut input = Cursor::new(b"\n\n\n\n".as_slice());

        let plan = collect_plan_with_authorization(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: false,
            },
            ExistingAuthorization::ReviewRequired,
            &mut input,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "disabled");
        assert_eq!(plan.credential_source, None);
        assert!(!plan.run_flight_check);
    }

    #[test]
    fn bound_environment_source_is_not_silently_replaced_by_keychain() {
        let mut current = aicoach_core::Config::default();
        current.ai.provider = "openai-compatible".to_owned();
        current.ai.base_url = "https://provider.example/v1".to_owned();
        current.ai.models.completion = "model-a".to_owned();
        current.ai.models.error_analysis = "model-a".to_owned();
        current.ai.models.chat = "model-a".to_owned();
        let mut input = Cursor::new(b"\n\n\n\n\n".as_slice());

        let plan = collect_plan_with_authorization(
            &current,
            CredentialAvailability {
                keychain: true,
                environment: true,
            },
            ExistingAuthorization::Bound(CredentialSource::Environment),
            &mut input,
            &mut Vec::new(),
        )
        .unwrap();

        assert_eq!(plan.config.ai.provider, "openai-compatible");
        assert_eq!(plan.credential_source, Some(CredentialSource::Environment));
        assert!(!plan.store_key);
        assert!(!plan.run_flight_check);
    }

    #[test]
    fn required_values_are_reprompted_and_chinese_answers_are_accepted() {
        let mut input = Cursor::new(b"\nvalue\n".as_slice());
        let mut output = Vec::new();
        assert_eq!(
            prompt_value(&mut input, &mut output, "Model ID", "", Language::English).unwrap(),
            "value"
        );
        assert!(
            String::from_utf8(output)
                .unwrap()
                .contains("A value is required")
        );

        assert!(
            prompt_yes_no(
                &mut Cursor::new("是\n".as_bytes()),
                &mut Vec::new(),
                "继续？",
                false,
                Language::Chinese
            )
            .unwrap()
        );
    }

    #[test]
    fn flight_check_timeout_is_bounded_without_lengthening_a_stricter_config() {
        let mut config = aicoach_core::Config::default();
        config.ai.timeouts_ms.chat = 90_000;
        let provider = flight_check_config(&config.ai);
        assert_eq!(provider.timeouts.chat, Duration::from_secs(10));
        assert_eq!(provider.retry.max_retries, 0);

        config.ai.timeouts_ms.chat = 2_000;
        let provider = flight_check_config(&config.ai);
        assert_eq!(provider.timeouts.chat, Duration::from_secs(2));
    }

    #[test]
    fn setup_flight_check_makes_only_one_attempt_on_retryable_failure() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 8_192];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .unwrap();
            listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + Duration::from_millis(300);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => return 2,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept retry: {error}"),
                }
            }
            1
        });

        let mut config = aicoach_core::Config::default();
        config.ai.base_url = format!("http://{address}/v1");
        config.ai.models.completion = "model".to_owned();
        config.ai.models.error_analysis = "model".to_owned();
        config.ai.models.chat = "model".to_owned();
        let error = run_flight_check(
            &config.ai,
            SecretString::from("local-placeholder-credential".to_owned()),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            AiError::HttpStatus {
                operation: aicoach_ai::AiOperation::Chat,
                status: 500
            }
        ));
        assert_eq!(server.join().unwrap(), 1);
    }
}
