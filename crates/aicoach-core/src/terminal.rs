//! Terminal-safe text normalization shared by every presentation surface.

#[derive(Clone, Copy)]
enum EscapeState {
    Text,
    Escape,
    Csi,
    Osc,
    OscEscape,
    ControlString,
    ControlStringEscape,
}

/// Removes ANSI/ECMA-48 escape sequences and terminal control characters.
///
/// When `multiline` is true, newlines and tabs are retained. All other C0/C1
/// controls and bidirectional formatting marks are removed so untrusted command
/// output cannot change a terminal title, emit a clickable link, rewrite a
/// report, manipulate the cursor, or visually reorder security-sensitive text.
pub fn strip_terminal_sequences(value: &str, multiline: bool) -> String {
    let mut output = String::with_capacity(value.len());
    let mut state = EscapeState::Text;
    for character in value.chars() {
        state = match state {
            EscapeState::Text => match character {
                '\u{1b}' => EscapeState::Escape,
                '\u{9b}' => EscapeState::Csi,
                '\u{9d}' => EscapeState::Osc,
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => EscapeState::ControlString,
                '\n' if multiline => {
                    output.push('\n');
                    EscapeState::Text
                }
                '\t' if multiline => {
                    output.push('\t');
                    EscapeState::Text
                }
                control if is_terminal_control(control) || is_bidi_control(control) => {
                    if !multiline && control.is_whitespace() {
                        output.push(' ');
                    }
                    EscapeState::Text
                }
                ordinary => {
                    output.push(ordinary);
                    EscapeState::Text
                }
            },
            EscapeState::Escape => match character {
                '[' => EscapeState::Csi,
                ']' => EscapeState::Osc,
                'P' | 'X' | '^' | '_' => EscapeState::ControlString,
                _ => EscapeState::Text,
            },
            EscapeState::Csi => {
                if character == '\u{1b}' {
                    EscapeState::Escape
                } else if ('@'..='~').contains(&character) {
                    EscapeState::Text
                } else {
                    EscapeState::Csi
                }
            }
            EscapeState::Osc => match character {
                '\u{7}' | '\u{9c}' => EscapeState::Text,
                '\u{1b}' => EscapeState::OscEscape,
                _ => EscapeState::Osc,
            },
            EscapeState::OscEscape => {
                if character == '\\' {
                    EscapeState::Text
                } else if character == '\u{1b}' {
                    EscapeState::OscEscape
                } else {
                    EscapeState::Osc
                }
            }
            EscapeState::ControlString => match character {
                '\u{7}' | '\u{9c}' => EscapeState::Text,
                '\u{1b}' => EscapeState::ControlStringEscape,
                _ => EscapeState::ControlString,
            },
            EscapeState::ControlStringEscape => {
                if character == '\\' {
                    EscapeState::Text
                } else if character == '\u{1b}' {
                    EscapeState::ControlStringEscape
                } else {
                    EscapeState::ControlString
                }
            }
        };
    }
    output
}

fn is_terminal_control(character: char) -> bool {
    character <= '\u{1f}' || ('\u{7f}'..='\u{9f}').contains(&character)
}

fn is_bidi_control(character: char) -> bool {
    matches!(
        character,
        '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_csi_osc_and_control_strings() {
        let value = concat!(
            "plain ",
            "\u{1b}[31mred\u{1b}[0m ",
            "\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{1b}\\ ",
            "\u{1b}Pignored\u{1b}\\done"
        );
        assert_eq!(strip_terminal_sequences(value, true), "plain red link done");
    }

    #[test]
    fn preserves_layout_only_when_requested() {
        assert_eq!(
            strip_terminal_sequences("one\ntwo\tthree", true),
            "one\ntwo\tthree"
        );
        assert_eq!(
            strip_terminal_sequences("one\ntwo\tthree", false),
            "one two three"
        );
    }

    #[test]
    fn strips_bidirectional_text_spoofing_controls() {
        assert_eq!(
            strip_terminal_sequences("safe\u{202e}txt\u{2066}end", true),
            "safetxtend"
        );
    }
}
