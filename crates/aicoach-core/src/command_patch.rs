//! Small, deterministic command diffs for explaining completion changes.

const MAX_LCS_TOKENS: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPatchHunk {
    pub removed: Vec<String>,
    pub added: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandPatch {
    pub hunks: Vec<CommandPatchHunk>,
}

impl CommandPatch {
    /// Build a token-level diff while preserving the user's original quoting.
    pub fn between(before: &str, after: &str) -> Self {
        if before == after {
            return Self::default();
        }
        let before_tokens = shell_tokens(before);
        let after_tokens = shell_tokens(after);
        if before_tokens == after_tokens {
            return Self::default();
        }
        if before_tokens.len() > MAX_LCS_TOKENS || after_tokens.len() > MAX_LCS_TOKENS {
            return Self {
                hunks: vec![CommandPatchHunk {
                    removed: vec![compact_whitespace(before)],
                    added: vec![compact_whitespace(after)],
                }],
            };
        }
        Self {
            hunks: lcs_hunks(&before_tokens, &after_tokens),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hunks.is_empty()
    }

    /// Render only changed tokens, bounded for a one-line terminal message.
    pub fn compact_summary(&self, max_chars: usize) -> String {
        let summary = self
            .hunks
            .iter()
            .map(
                |hunk| match (hunk.removed.is_empty(), hunk.added.is_empty()) {
                    (false, false) => {
                        format!("− {} → + {}", hunk.removed.join(" "), hunk.added.join(" "))
                    }
                    (false, true) => format!("− {}", hunk.removed.join(" ")),
                    (true, false) => format!("+ {}", hunk.added.join(" ")),
                    (true, true) => String::new(),
                },
            )
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        truncate_with_ellipsis(&summary, max_chars)
    }
}

/// Split on unquoted whitespace without normalizing or discarding quotes.
/// This intentionally is not a shell parser: an unfinished quote is ordinary
/// while a user is typing, and preserving it makes the displayed patch honest.
fn shell_tokens(source: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut quote = None;
    let mut escaped = false;
    for character in source.chars() {
        if escaped {
            token.push(character);
            escaped = false;
            continue;
        }
        match quote {
            Some('\'') => {
                token.push(character);
                if character == '\'' {
                    quote = None;
                }
            }
            Some('"') => {
                token.push(character);
                if character == '"' {
                    quote = None;
                } else if character == '\\' {
                    escaped = true;
                }
            }
            _ if character.is_whitespace() => {
                if !token.is_empty() {
                    tokens.push(std::mem::take(&mut token));
                }
            }
            _ => {
                token.push(character);
                if matches!(character, '\'' | '"') {
                    quote = Some(character);
                } else if character == '\\' {
                    escaped = true;
                }
            }
        }
    }
    if !token.is_empty() {
        tokens.push(token);
    }
    tokens
}

fn lcs_hunks(before: &[String], after: &[String]) -> Vec<CommandPatchHunk> {
    let mut lengths = vec![vec![0_usize; after.len() + 1]; before.len() + 1];
    for before_index in (0..before.len()).rev() {
        for after_index in (0..after.len()).rev() {
            lengths[before_index][after_index] = if before[before_index] == after[after_index] {
                lengths[before_index + 1][after_index + 1] + 1
            } else {
                lengths[before_index + 1][after_index].max(lengths[before_index][after_index + 1])
            };
        }
    }

    let mut hunks = Vec::new();
    let mut current = CommandPatchHunk {
        removed: Vec::new(),
        added: Vec::new(),
    };
    let mut before_index = 0;
    let mut after_index = 0;
    while before_index < before.len() || after_index < after.len() {
        if before_index < before.len()
            && after_index < after.len()
            && before[before_index] == after[after_index]
        {
            push_hunk(&mut hunks, &mut current);
            before_index += 1;
            after_index += 1;
        } else if before_index < before.len()
            && (after_index == after.len()
                || lengths[before_index + 1][after_index] >= lengths[before_index][after_index + 1])
        {
            current.removed.push(before[before_index].clone());
            before_index += 1;
        } else {
            current.added.push(after[after_index].clone());
            after_index += 1;
        }
    }
    push_hunk(&mut hunks, &mut current);
    hunks
}

fn push_hunk(hunks: &mut Vec<CommandPatchHunk>, current: &mut CommandPatchHunk) {
    if current.removed.is_empty() && current.added.is_empty() {
        return;
    }
    hunks.push(CommandPatchHunk {
        removed: std::mem::take(&mut current.removed),
        added: std::mem::take(&mut current.added),
    });
}

fn compact_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_with_ellipsis(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_owned();
    }
    if max_chars == 0 {
        return String::new();
    }
    let mut result = value
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    result.push('…');
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_replacement_insertion_and_removal() {
        let replacement = CommandPatch::between("git pul origin main", "git pull origin main");
        assert_eq!(replacement.compact_summary(200), "− pul → + pull");

        let insertion = CommandPatch::between("git push", "git push --force-with-lease");
        assert_eq!(insertion.compact_summary(200), "+ --force-with-lease");

        let removal = CommandPatch::between("cargo test --release", "cargo test");
        assert_eq!(removal.compact_summary(200), "− --release");
    }

    #[test]
    fn preserves_quoting_changes_and_unfinished_input() {
        let quoting = CommandPatch::between("echo \"$HOME\"", "echo '$HOME'");
        assert_eq!(quoting.compact_summary(200), "− \"$HOME\" → + '$HOME'");

        let unfinished =
            CommandPatch::between("printf 'hello world", "printf '%s\\n' 'hello world'");
        assert!(!unfinished.is_empty());
        assert!(unfinished.compact_summary(200).contains("'hello world"));
    }

    #[test]
    fn reports_separate_hunks_and_unicode_safely() {
        let patch = CommandPatch::between("git pul main --forc", "git pull main --force");
        assert_eq!(patch.hunks.len(), 2);
        assert_eq!(
            patch.compact_summary(200),
            "− pul → + pull; − --forc → + --force"
        );
        assert_eq!(
            CommandPatch::between("echo 你", "echo 你好").compact_summary(8),
            "− 你 → +…"
        );
    }

    #[test]
    fn ignores_semantically_empty_whitespace_changes() {
        assert!(CommandPatch::between("git  status", "git status").is_empty());
        assert!(CommandPatch::between("same", "same").is_empty());
    }
}
