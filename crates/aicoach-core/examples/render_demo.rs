use aicoach_core::{
    AnalysisCoverage, AnalysisInput, CommandPatch, EffectAction, LocalAnalyzer, PrivacyRedactor,
    PrivilegeRequirement, RecoveryProspect, RiskLevel, SafetyEngine,
};
use std::{env, fs, path::Path, process::ExitCode};

const TEMPLATE: &str = include_str!("workflow.svg.template");

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("render demo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let mut input = AnalysisInput::new("git pul origin main", 1, Path::new("/demo/project"));
    "git: 'pul' is not a git command".clone_into(&mut input.stderr);
    let diagnosis = LocalAnalyzer::new().analyze(&input);
    let suggestion = diagnosis
        .suggested_command
        .as_deref()
        .ok_or_else(|| "local analyzer did not produce the expected suggestion".to_owned())?;
    if diagnosis.needs_ai {
        return Err("the showcased spelling correction is no longer fully local".to_owned());
    }
    let patch = CommandPatch::between(&input.command, suggestion).compact_summary(160);
    if patch.is_empty() {
        return Err("Command Patch did not explain the showcased correction".to_owned());
    }
    let risk = SafetyEngine::new().risk_lens("git reset --hard");
    if risk.level != Some(RiskLevel::High)
        || risk.coverage != AnalysisCoverage::Recognized
        || !risk.rule_ids.iter().any(|rule| rule == "git.reset-hard")
    {
        return Err(
            "the showcased Risk Lens scenario no longer has its expected evidence".to_owned(),
        );
    }
    let effect = risk
        .effects
        .first()
        .ok_or_else(|| "Risk Lens did not produce an effect".to_owned())?;
    let redacted = PrivacyRedactor::default().redact("PASSWORD=hunter2-demo");
    if redacted != "PASSWORD=[REDACTED]" {
        return Err("the showcased password was not fully redacted".to_owned());
    }
    let rules = risk.rule_ids.join(", ");

    let mut svg = TEMPLATE.to_owned();
    for (marker, value) in [
        ("{{DIAGNOSIS_TITLE}}", diagnosis.title.as_str()),
        ("{{DIAGNOSIS_MESSAGE}}", diagnosis.message.as_str()),
        ("{{SUGGESTION}}", suggestion),
        ("{{PATCH}}", patch.as_str()),
        ("{{RISK_LEVEL}}", risk_level(risk.level)),
        ("{{COVERAGE}}", coverage(risk.coverage)),
        ("{{RISK_ACTION}}", effect_action(effect.action)),
        ("{{RISK_TARGET}}", effect.target.as_str()),
        ("{{PRIVILEGE}}", privilege(risk.privilege)),
        ("{{RECOVERY}}", recovery(risk.recovery)),
        ("{{RULES}}", rules.as_str()),
        ("{{REDACTED}}", redacted.as_str()),
    ] {
        svg = svg.replace(marker, &xml_escape(value));
    }
    if svg.contains("{{") {
        return Err("the SVG template still contains an unresolved marker".to_owned());
    }

    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [output] => {
            fs::write(output, svg).map_err(|error| format!("write {output}: {error}"))?;
            println!("wrote {output}");
        }
        [flag, existing] if flag == "--check" => {
            let committed = fs::read_to_string(existing)
                .map_err(|error| format!("read {existing}: {error}"))?;
            if committed != svg {
                return Err(format!(
                    "{existing} is stale; regenerate it with `cargo run -p aicoach-core --example render_demo -- {existing}`"
                ));
            }
            println!("demo asset is reproducible and current");
        }
        _ => {
            return Err(
                "usage: render_demo <output.svg> | render_demo --check <existing.svg>".to_owned(),
            );
        }
    }
    Ok(())
}

fn risk_level(level: Option<RiskLevel>) -> &'static str {
    match level {
        Some(RiskLevel::Low) => "LOW",
        Some(RiskLevel::Medium) => "MEDIUM",
        Some(RiskLevel::High) => "HIGH",
        Some(RiskLevel::Critical) => "CRITICAL",
        None => "UNRATED",
    }
}

fn coverage(value: AnalysisCoverage) -> &'static str {
    match value {
        AnalysisCoverage::Recognized => "RECOGNIZED",
        AnalysisCoverage::Partial => "PARTIAL COVERAGE",
        AnalysisCoverage::Unknown => "UNKNOWN COMMAND",
    }
}

fn effect_action(value: EffectAction) -> &'static str {
    match value {
        EffectAction::NoneDetected => "NO PERSISTENT CHANGE",
        EffectAction::Read => "READ",
        EffectAction::Create => "CREATE",
        EffectAction::Modify => "MODIFY",
        EffectAction::Delete => "DELETE",
        EffectAction::Execute => "EXECUTE / OPEN",
        EffectAction::Network => "NETWORK / REMOTE",
        EffectAction::Process => "AFFECT PROCESS",
        EffectAction::System => "AFFECT SYSTEM",
        EffectAction::Unknown => "UNKNOWN EFFECT",
    }
}

fn privilege(value: PrivilegeRequirement) -> &'static str {
    match value {
        PrivilegeRequirement::CurrentUser => "current user · no explicit elevation",
        PrivilegeRequirement::Unknown => "unknown to local rules",
        PrivilegeRequirement::ElevatedLikely => "device or elevated access likely",
        PrivilegeRequirement::Administrator => "administrator via sudo",
    }
}

fn recovery(value: RecoveryProspect) -> &'static str {
    match value {
        RecoveryProspect::NotApplicable => "none needed for detected effects",
        RecoveryProspect::Reversible => "usually reversible if prior state is known",
        RecoveryProspect::Limited => "limited · may need backup, reflog, or remote history",
        RecoveryProspect::Unknown => "unknown · verify backup and rollback first",
        RecoveryProspect::Irreversible => "irreversible · assume there is no undo",
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_blocks_dynamic_svg_markup() {
        assert_eq!(
            xml_escape("<x a='b'>&\""),
            "&lt;x a=&apos;b&apos;&gt;&amp;&quot;"
        );
    }
}
