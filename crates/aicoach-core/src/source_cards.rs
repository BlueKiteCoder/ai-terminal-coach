//! Safe plans and parsers for evidence collected from documentation installed on this Mac.
//!
//! This module never starts a process. It turns recognized commands into a
//! small allowlisted query plan and converts bounded command output into
//! terminal-safe cards. The daemon is responsible for executing the plan with
//! an absolute binary path, a timeout, and an output limit.

use serde::{Deserialize, Serialize};

use crate::{
    safety::{command_index, executable_name, lenient_tokens, split_shell_commands},
    strip_terminal_sequences,
};

const MAX_QUERIES: usize = 2;
const MAX_EXCERPT_CHARS: usize = 320;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceOrigin {
    CommandHelp,
    ManPage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCard {
    pub origin: SourceOrigin,
    /// A human-readable, reproducible local command such as `git reset -h`.
    pub reference: String,
    /// The option or manual section used to select the excerpt.
    pub section: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SourceInvocation {
    /// Only absolute, product-owned allowlist entries may use this variant.
    CommandHelp {
        program: &'static str,
        arguments: Vec<String>,
    },
    ManPage {
        topic: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceQuery {
    pub invocation: SourceInvocation,
    pub reference: String,
    pub section: String,
}

impl SourceQuery {
    fn git(subcommand: &str, section: &str) -> Self {
        Self {
            invocation: SourceInvocation::CommandHelp {
                program: "/usr/bin/git",
                arguments: vec![subcommand.to_owned(), "-h".to_owned()],
            },
            reference: format!("git {subcommand} -h"),
            section: section.to_owned(),
        }
    }

    fn man(topic: &'static str, section: &str) -> Self {
        Self {
            invocation: SourceInvocation::ManPage { topic },
            reference: format!("man {topic}"),
            section: section.to_owned(),
        }
    }
}

/// Build at most two documentation queries from recognized, allowlisted
/// commands and destructive-rule identifiers.
pub fn source_queries(command: &str, rule_ids: &[String]) -> Vec<SourceQuery> {
    let mut queries = Vec::new();
    for rule_id in rule_ids {
        if let Some(query) = query_for_rule(rule_id, command) {
            push_query(&mut queries, query);
        }
        if queries.len() == MAX_QUERIES {
            return queries;
        }
    }

    for segment in split_shell_commands(command) {
        let tokens = shell_words::split(&segment).unwrap_or_else(|_| lenient_tokens(&segment));
        let Some(index) = command_index(&tokens) else {
            continue;
        };
        let executable = executable_name(&tokens[index]);
        let arguments = &tokens[index + 1..];
        if let Some(query) = query_for_command(&executable, arguments) {
            push_query(&mut queries, query);
        }
        if queries.len() == MAX_QUERIES {
            break;
        }
    }
    queries
}

fn push_query(queries: &mut Vec<SourceQuery>, query: SourceQuery) {
    if !queries
        .iter()
        .any(|existing| existing.invocation == query.invocation)
    {
        queries.push(query);
    }
}

fn query_for_rule(rule_id: &str, command: &str) -> Option<SourceQuery> {
    match rule_id {
        "git.reset-hard" => Some(SourceQuery::git("reset", "--hard")),
        "git.clean-force" => Some(SourceQuery::git("clean", "-f")),
        "git.push-force" => Some(SourceQuery::git("push", "-f")),
        "git.push-force-with-lease" => Some(SourceQuery::git("push", "force-with-lease")),
        rule if rule.starts_with("rm.") => Some(SourceQuery::man("rm", "-R")),
        "disk.dd-device" | "file.dd-overwrite" => Some(SourceQuery::man("dd", "of=")),
        "disk.diskutil-erase" => Some(SourceQuery::man(
            "diskutil",
            diskutil_section(command).unwrap_or("eraseDisk"),
        )),
        "disk.diskutil-volume" => Some(SourceQuery::man(
            "diskutil",
            diskutil_section(command).unwrap_or("DESCRIPTION"),
        )),
        "permissions.world-writable" => Some(SourceQuery::man("chmod", "MODES")),
        "permissions.recursive-owner" => Some(SourceQuery::man("chown", "-R")),
        "process.sigkill" => Some(SourceQuery::man("kill", "-s")),
        "find.delete" => Some(SourceQuery::man("find", "-delete")),
        "system.power" => Some(SourceQuery::man("shutdown", "DESCRIPTION")),
        "file.truncate" => Some(SourceQuery::man("truncate", "-s")),
        _ => None,
    }
}

fn query_for_command(executable: &str, arguments: &[String]) -> Option<SourceQuery> {
    match executable {
        "git" => {
            let subcommand = first_operand(arguments)?;
            if !matches!(
                subcommand,
                "add"
                    | "blame"
                    | "branch"
                    | "checkout"
                    | "clean"
                    | "commit"
                    | "diff"
                    | "fetch"
                    | "log"
                    | "merge"
                    | "pull"
                    | "push"
                    | "rebase"
                    | "reset"
                    | "restore"
                    | "rev-parse"
                    | "show"
                    | "status"
                    | "switch"
            ) {
                return None;
            }
            let section = preferred_option(arguments).unwrap_or_else(|| "usage".to_owned());
            Some(SourceQuery::git(subcommand, &section))
        }
        "rm" => Some(SourceQuery::man("rm", "DESCRIPTION")),
        "rmdir" => Some(SourceQuery::man("rmdir", "DESCRIPTION")),
        "unlink" => Some(SourceQuery::man("unlink", "DESCRIPTION")),
        "cp" => Some(SourceQuery::man("cp", "DESCRIPTION")),
        "mv" => Some(SourceQuery::man("mv", "DESCRIPTION")),
        "mkdir" => Some(SourceQuery::man("mkdir", "DESCRIPTION")),
        "chmod" => Some(SourceQuery::man("chmod", "DESCRIPTION")),
        "chown" => Some(SourceQuery::man("chown", "DESCRIPTION")),
        "chgrp" => Some(SourceQuery::man("chgrp", "DESCRIPTION")),
        "dd" => Some(SourceQuery::man("dd", "DESCRIPTION")),
        "defaults" => Some(SourceQuery::man("defaults", "DESCRIPTION")),
        "diskutil" => Some(SourceQuery::man(
            "diskutil",
            diskutil_section_from_args(arguments).unwrap_or("DESCRIPTION"),
        )),
        "launchctl" => Some(SourceQuery::man("launchctl", "DESCRIPTION")),
        "find" => Some(SourceQuery::man("find", "DESCRIPTION")),
        "kill" => Some(SourceQuery::man("kill", "DESCRIPTION")),
        "killall" => Some(SourceQuery::man("killall", "DESCRIPTION")),
        "pkill" => Some(SourceQuery::man("pkill", "DESCRIPTION")),
        "shutdown" => Some(SourceQuery::man("shutdown", "DESCRIPTION")),
        "reboot" => Some(SourceQuery::man("reboot", "DESCRIPTION")),
        "halt" => Some(SourceQuery::man("halt", "DESCRIPTION")),
        "docker" => Some(SourceQuery::man("docker", "DESCRIPTION")),
        "kubectl" => Some(SourceQuery::man("kubectl", "DESCRIPTION")),
        _ => None,
    }
}

fn first_operand(arguments: &[String]) -> Option<&str> {
    arguments
        .iter()
        .find(|argument| !argument.starts_with('-'))
        .map(String::as_str)
}

fn preferred_option(arguments: &[String]) -> Option<String> {
    arguments
        .iter()
        .rev()
        .find(|argument| argument.starts_with('-') && argument.as_str() != "--")
        .and_then(|argument| argument.split('=').next())
        .filter(|option| {
            option.len() <= 64
                && option
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        })
        .map(ToOwned::to_owned)
}

fn diskutil_section(command: &str) -> Option<&'static str> {
    for segment in split_shell_commands(command) {
        let tokens = shell_words::split(&segment).unwrap_or_else(|_| lenient_tokens(&segment));
        let Some(index) = command_index(&tokens) else {
            continue;
        };
        if executable_name(&tokens[index]) == "diskutil" {
            return diskutil_section_from_args(&tokens[index + 1..]);
        }
    }
    None
}

fn diskutil_section_from_args(arguments: &[String]) -> Option<&'static str> {
    arguments
        .iter()
        .filter(|argument| !argument.starts_with('-'))
        .find_map(|argument| match argument.to_ascii_lowercase().as_str() {
            "erasedisk" => Some("eraseDisk"),
            "erasevolume" => Some("eraseVolume"),
            "partitiondisk" => Some("partitionDisk"),
            "zerodisk" => Some("zeroDisk"),
            "randomdisk" => Some("randomDisk"),
            "addvolume" => Some("addVolume"),
            "deletevolume" => Some("deleteVolume"),
            "deletecontainer" => Some("deleteContainer"),
            "resizecontainer" => Some("resizeContainer"),
            "mount" => Some("mount"),
            "unmount" => Some("unmount"),
            "eject" => Some("eject"),
            "repairdisk" => Some("repairDisk"),
            "repairvolume" => Some("repairVolume"),
            "verifydisk" => Some("verifyDisk"),
            "verifyvolume" => Some("verifyVolume"),
            _ => None,
        })
}

/// Convert the bounded output of an allowlisted local documentation command
/// into a terminal-safe card.
pub fn source_card_from_output(query: &SourceQuery, output: &str) -> Option<SourceCard> {
    let normalized = normalize_document(output);
    let lines = normalized.lines().collect::<Vec<_>>();
    let index = find_section(&lines, &query.section)?;
    let excerpt = collect_excerpt(&lines, index, &query.section);
    if excerpt.is_empty() {
        return None;
    }
    Some(SourceCard {
        origin: match query.invocation {
            SourceInvocation::CommandHelp { .. } => SourceOrigin::CommandHelp,
            SourceInvocation::ManPage { .. } => SourceOrigin::ManPage,
        },
        reference: query.reference.clone(),
        section: query.section.clone(),
        excerpt,
    })
}

fn normalize_document(output: &str) -> String {
    let mut without_overstrike = String::with_capacity(output.len());
    for character in output.chars() {
        if character == '\u{8}' {
            without_overstrike.pop();
        } else {
            without_overstrike.push(character);
        }
    }
    strip_terminal_sequences(&without_overstrike, true)
}

fn find_section(lines: &[&str], section: &str) -> Option<usize> {
    let section_lower = section.to_ascii_lowercase();
    lines.iter().position(|line| {
        let trimmed = line.trim();
        let lower = trimmed.to_ascii_lowercase();
        if section_lower == "usage" {
            lower.starts_with("usage:")
        } else if section
            .chars()
            .all(|character| character.is_ascii_uppercase())
        {
            trimmed == section
        } else {
            lower.starts_with(&section_lower)
                || (!lower.starts_with("usage:") && lower.contains(&section_lower))
        }
    })
}

fn collect_excerpt(lines: &[&str], index: usize, section: &str) -> String {
    let heading = section
        .chars()
        .all(|character| character.is_ascii_uppercase());
    let mut parts = Vec::new();
    for (offset, line) in lines.iter().skip(index + usize::from(heading)).enumerate() {
        let mut compact = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if compact.is_empty() {
            if !parts.is_empty() {
                break;
            }
            continue;
        }
        if !heading && offset == 0 {
            let Some(description) = option_description(line, section) else {
                continue;
            };
            compact = description;
        } else if looks_like_new_section(&compact) {
            break;
        }
        parts.push(compact);
        if parts.len() == 4 {
            break;
        }
    }
    truncate_chars(&parts.join(" "), MAX_EXCERPT_CHARS)
}

fn option_description(line: &str, section: &str) -> Option<String> {
    let trimmed = line.trim();
    if section.eq_ignore_ascii_case("usage") {
        return trimmed
            .get("usage:".len()..)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(ToOwned::to_owned);
    }
    let boundary = [trimmed.find("  "), trimmed.find('\t')]
        .into_iter()
        .flatten()
        .min()?;
    let description = trimmed.get(boundary..)?.trim();
    (!description.is_empty()).then(|| description.to_owned())
}

fn looks_like_new_section(line: &str) -> bool {
    line.starts_with('-')
        || (line.len() < 48 && line.chars().all(|c| c.is_ascii_uppercase() || c == ' '))
}

fn truncate_chars(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_owned();
    }
    let mut truncated = value
        .chars()
        .take(limit.saturating_sub(2))
        .collect::<String>();
    truncated.push('…');
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn destructive_git_query_uses_fixed_binary_and_specific_help_section() {
        let queries = source_queries("sudo git reset --hard", &["git.reset-hard".to_owned()]);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].reference, "git reset -h");
        assert_eq!(queries[0].section, "--hard");
        assert_eq!(
            queries[0].invocation,
            SourceInvocation::CommandHelp {
                program: "/usr/bin/git",
                arguments: vec!["reset".to_owned(), "-h".to_owned()],
            }
        );
    }

    #[test]
    fn arbitrary_executables_and_git_subcommands_never_become_processes() {
        assert!(source_queries("./deploy --help", &[]).is_empty());
        assert!(source_queries("git credential fill", &[]).is_empty());
        assert!(source_queries("company-git reset --hard", &[]).is_empty());
    }

    #[test]
    fn supported_profile_without_a_rule_still_gets_local_evidence() {
        let queries = source_queries("git status --short", &[]);
        assert_eq!(queries.len(), 1);
        assert_eq!(queries[0].reference, "git status -h");
        assert_eq!(queries[0].section, "--short");

        let normalized = source_queries("git log --format=%H", &[]);
        assert_eq!(normalized[0].section, "--format");
        let oversized = format!("git status --{}", "x".repeat(80));
        assert_eq!(source_queries(&oversized, &[])[0].section, "usage");
    }

    #[test]
    fn output_parser_extracts_one_sanitized_bounded_section() {
        let query = SourceQuery::git("reset", "--hard");
        let output = concat!(
            "usage: git reset [--hard]\n\n",
            "    _\u{8}-_\u{8}-hard   reset HEAD, index and working tree\n",
            "             and discard tracked changes\u{1b}]0;spoof\u{7}\n\n",
            "    --merge  reset with a merge\n"
        );
        let card = source_card_from_output(&query, output).expect("source card");
        assert_eq!(card.origin, SourceOrigin::CommandHelp);
        assert_eq!(
            card.excerpt,
            "reset HEAD, index and working tree and discard tracked changes"
        );
        assert!(!card.excerpt.contains('\u{1b}'));
    }

    #[test]
    fn man_headings_select_the_first_body_paragraph() {
        let query = SourceQuery::man("rm", "DESCRIPTION");
        let card = source_card_from_output(
            &query,
            "NAME\n rm - remove files\n\nDESCRIPTION\n The rm utility removes entries.\n It never uses the trash.\n\nOPTIONS\n -f force\n",
        )
        .expect("source card");
        assert_eq!(
            card.excerpt,
            "The rm utility removes entries. It never uses the trash."
        );
    }
}
