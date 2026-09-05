use super::{
    Check, Paths, atomic_write, collect_doctor_checks, ensure_macos, public_terminal_name,
};
use aicoach_core::Config;
use aicoach_ipc::PROTOCOL_VERSION;
use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use clap::Args;
use std::{
    env,
    io::Write,
    path::PathBuf,
    process::{Command, Stdio},
};

#[derive(Args, Debug)]
pub(super) struct SupportArgs {
    /// Copy the Markdown report to the macOS clipboard.
    #[arg(long)]
    pub(super) copy: bool,
    /// Write the report to a private file instead of stdout.
    #[arg(short, long)]
    pub(super) output: Option<PathBuf>,
}

struct PublicFacts {
    macos: String,
    architecture: &'static str,
    zsh: String,
    terminal: &'static str,
    language: &'static str,
    provider: &'static str,
    chinese: bool,
}

pub(super) fn export(paths: &Paths, args: &SupportArgs) -> Result<()> {
    ensure_macos()?;
    let checks = collect_doctor_checks(paths);
    let facts = collect_public_facts(paths);
    let report = render_support_report(
        &facts,
        &checks,
        &Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    );

    let mut delivered = false;
    if let Some(output) = args.output.as_ref() {
        let output = if output.is_absolute() {
            output.clone()
        } else {
            env::current_dir()
                .context("resolve support report output directory")?
                .join(output)
        };
        atomic_write(&output, &report, 0o600)?;
        eprintln!("Support report written to {}", output.display());
        delivered = true;
    }
    if args.copy {
        copy_to_clipboard(&report)?;
        eprintln!("Support report copied to the clipboard");
        delivered = true;
    }
    if !delivered {
        print!("{report}");
    }

    if checks
        .iter()
        .any(|check| check.required && check.status == "fail")
    {
        bail!("support report contains required checks that need attention")
    }
    Ok(())
}

fn collect_public_facts(paths: &Paths) -> PublicFacts {
    let terminal = env::var("TERM_PROGRAM")
        .ok()
        .as_deref()
        .and_then(public_terminal_name)
        .unwrap_or("Other / unrecognized");
    let (language, provider, chinese) =
        Config::load_from(&paths.config).map_or(("unreadable", "unreadable", false), |config| {
            let language = match config.coach.language.as_str() {
                "en-US" => "English (en-US)",
                "zh-CN" => "简体中文 (zh-CN)",
                _ => "other (value omitted)",
            };
            let provider = match config.ai.provider.as_str() {
                "disabled" => "disabled (local-only)",
                "openai-compatible" => "OpenAI-compatible (endpoint omitted)",
                _ => "other (details omitted)",
            };
            (language, provider, config.coach.language == "zh-CN")
        });

    PublicFacts {
        macos: command_output("/usr/bin/sw_vers", &["-productVersion"])
            .unwrap_or_else(|| "unavailable".to_owned()),
        architecture: match env::consts::ARCH {
            "aarch64" => "Apple Silicon (arm64)",
            "x86_64" => "Intel (x86_64)",
            _ => "other / unrecognized",
        },
        zsh: command_output("/bin/zsh", &["--version"]).unwrap_or_else(|| "unavailable".to_owned()),
        terminal,
        language,
        provider,
        chinese,
    }
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let line = String::from_utf8(output.stdout).ok()?;
    let line = line.lines().next()?.trim();
    (!line.is_empty() && line.len() <= 120).then(|| line.to_owned())
}

fn render_support_report(facts: &PublicFacts, checks: &[Check], generated_at: &str) -> String {
    use std::fmt::Write as _;

    let mut report = String::new();
    let _ = writeln!(
        report,
        "# {}\n",
        if facts.chinese {
            "AI Terminal Coach 支持报告"
        } else {
            "AI Terminal Coach Support Report"
        }
    );
    let _ = writeln!(
        report,
        "> {}\n",
        if facts.chinese {
            "本报告完全在本机生成，不调用 AI 或网络；不包含用户名、主机名、路径、session ID、命令、聊天、输出、日志、API endpoint、模型名或凭据。公开发布前仍请检查。"
        } else {
            "Generated locally without an AI or network request. This report excludes usernames, hostnames, paths, session IDs, commands, chat, output, logs, API endpoints, model names, and credentials. Review it before posting."
        }
    );
    let _ = writeln!(
        report,
        "## {}\n",
        if facts.chinese {
            "运行环境"
        } else {
            "Runtime"
        }
    );
    let _ = writeln!(
        report,
        "- {}: `{generated_at}`",
        if facts.chinese {
            "生成时间"
        } else {
            "Generated"
        }
    );
    let _ = writeln!(
        report,
        "- AI Terminal Coach: `{}` (IPC protocol `{PROTOCOL_VERSION}`)",
        env!("CARGO_PKG_VERSION")
    );
    let _ = writeln!(report, "- macOS: `{}`", facts.macos);
    let _ = writeln!(
        report,
        "- {}: `{}`",
        if facts.chinese {
            "架构"
        } else {
            "Architecture"
        },
        facts.architecture
    );
    let _ = writeln!(report, "- Zsh: `{}`", facts.zsh);
    let _ = writeln!(
        report,
        "- {}: `{}`",
        if facts.chinese { "终端" } else { "Terminal" },
        facts.terminal
    );
    let _ = writeln!(
        report,
        "- {}: `{}`",
        if facts.chinese {
            "界面语言"
        } else {
            "UI language"
        },
        facts.language
    );
    let _ = writeln!(report, "- Provider: `{}`", facts.provider);

    let _ = writeln!(
        report,
        "\n## {}\n",
        if facts.chinese {
            "能力检查"
        } else {
            "Capability checks"
        }
    );
    let _ = writeln!(
        report,
        "| {} | {} | {} |",
        if facts.chinese {
            "能力"
        } else {
            "Capability"
        },
        if facts.chinese { "必需" } else { "Required" },
        if facts.chinese { "结果" } else { "Result" }
    );
    let _ = writeln!(report, "|---|:---:|:---:|");
    for check in checks {
        let required = match (facts.chinese, check.required) {
            (true, true) => "是",
            (true, false) => "否",
            (false, true) => "yes",
            (false, false) => "no",
        };
        let _ = writeln!(
            report,
            "| {} | {required} | {} |",
            public_check_name(check.name, facts.chinese),
            public_check_status(check.status, facts.chinese)
        );
    }

    let _ = writeln!(
        report,
        "\n## {}\n",
        if facts.chinese {
            "终端手动确认"
        } else {
            "Manual terminal confirmation"
        }
    );
    let _ = writeln!(
        report,
        "{}\n",
        if facts.chinese {
            "提交兼容性报告前，只勾选你在这个终端里亲自测试过的项目："
        } else {
            "Mark only what you personally tested in this terminal before submitting a compatibility report:"
        }
    );
    let _ = writeln!(
        report,
        "- [ ] {}",
        if facts.chinese {
            "`Option+Tab` 显示仅插入、不执行的补全（需要 Provider）。"
        } else {
            "`Option+Tab` shows an insert-only completion (provider required)."
        }
    );
    let _ = writeln!(
        report,
        "- [ ] {}",
        if facts.chinese {
            "`Option+/` 显示流式回答（需要 Provider）。"
        } else {
            "`Option+/` shows a streaming answer (provider required)."
        }
    );
    let _ = writeln!(
        report,
        "- [ ] {}",
        if facts.chinese {
            "`Option+R` 打开不依赖 Provider 的 Risk Lens。"
        } else {
            "`Option+R` opens the provider-free Risk Lens."
        }
    );
    let _ = writeln!(
        report,
        "- [ ] {}",
        if facts.chinese {
            "`Option+Space` 切换 Coach 窗口或使用文档说明的回退方式。"
        } else {
            "`Option+Space` toggles the Coach window or documented fallback."
        }
    );
    let _ = writeln!(
        report,
        "- [ ] {}",
        if facts.chinese {
            "普通输入和 Enter 的行为与安装前完全一致。"
        } else {
            "Normal typing and Enter behave exactly as before installation."
        }
    );
    report
}

fn public_check_name(name: &str, chinese: bool) -> &str {
    if !chinese {
        return name;
    }
    match name {
        "Zsh integration" => "Zsh 集成",
        "Config" => "配置",
        "Daemon" => "后台服务",
        "Socket" => "IPC Socket",
        "AI credential" => "AI 凭据",
        "Terminal" => "终端",
        "Global hotkey" => "全局快捷键",
        "Key bindings" => "按键绑定",
        "Config values" => "配置值",
        _ => name,
    }
}

fn public_check_status(status: &str, chinese: bool) -> &str {
    if !chinese {
        return status;
    }
    match status {
        "ok" => "正常",
        "warn" => "警告",
        "fail" => "失败",
        _ => "未知",
    }
}

fn copy_to_clipboard(contents: &str) -> Result<()> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .context("start macOS clipboard service")?;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("macOS clipboard stdin is unavailable"))?
        .write_all(contents.as_bytes())
        .context("write support report to macOS clipboard")?;
    let status = child.wait().context("wait for macOS clipboard service")?;
    if !status.success() {
        bail!("macOS clipboard service exited with {status}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> PublicFacts {
        PublicFacts {
            macos: "15.6".to_owned(),
            architecture: "Apple Silicon (arm64)",
            zsh: "zsh 5.9 (arm64-apple-darwin24.0)".to_owned(),
            terminal: "Terminal.app",
            language: "English (en-US)",
            provider: "OpenAI-compatible (endpoint omitted)",
            chinese: false,
        }
    }

    #[test]
    fn public_report_never_renders_private_check_details() {
        let checks = vec![
            Check {
                name: "Config",
                status: "ok",
                detail: "/Users/alice/private/config.toml".to_owned(),
                required: true,
            },
            Check {
                name: "AI credential",
                status: "ok",
                detail: "secret-provider-key-is-present".to_owned(),
                required: false,
            },
        ];
        let report = render_support_report(&facts(), &checks, "2026-09-06T00:00:00Z");

        assert!(report.contains("# AI Terminal Coach Support Report"));
        assert!(report.contains("| Config | yes | ok |"));
        assert!(report.contains("endpoint omitted"));
        assert!(report.contains("Normal typing and Enter"));
        assert!(!report.contains("/Users/alice"));
        assert!(!report.contains("secret-provider-key"));
    }

    #[test]
    fn terminal_names_are_allowlisted_instead_of_echoed() {
        assert_eq!(public_terminal_name("Apple_Terminal"), Some("Terminal.app"));
        assert_eq!(public_terminal_name("Alacritty"), Some("Alacritty"));
        assert_eq!(public_terminal_name("private-company-terminal"), None);
    }

    #[test]
    fn chinese_configuration_localizes_the_same_safe_report() {
        let mut facts = facts();
        facts.language = "简体中文 (zh-CN)";
        facts.chinese = true;
        let checks = vec![Check {
            name: "Daemon",
            status: "warn",
            detail: "/Users/alice/private.sock".to_owned(),
            required: false,
        }];

        let report = render_support_report(&facts, &checks, "2026-09-06T00:00:00Z");
        assert!(report.contains("# AI Terminal Coach 支持报告"));
        assert!(report.contains("| 后台服务 | 否 | 警告 |"));
        assert!(report.contains("普通输入和 Enter"));
        assert!(!report.contains("/Users/alice"));
    }
}
