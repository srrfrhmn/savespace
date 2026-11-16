//! The copy checks (design doc §7.5 Validator, Appendix B).
//!
//! Templates are the only text source in the MVP (D10). Tests run every
//! template through these checks, so a copy change that breaks the style
//! guide fails CI. The same checks guard LLM text when it is turned on.

use crate::model::Kind;

/// Banned words and phrases (Appendix B.1). Matched as whole words, not
/// case-sensitive.
pub const BANNED: &[&str] = &[
    "overspent",
    "failed",
    "bad",
    "careless",
    "wasted",
    "warning",
    "danger",
    "urgent",
    "alarm",
    "broke",
    "irresponsible",
    "guilt",
    "shame",
    "should",
    "must",
    "always",
    "never",
    "don't worry",
    "do not worry",
    "relax",
    "calm down",
    "great job",
    "amazing",
    "average",
];

/// Words a `reassurance` text must not use (kind consistency).
const CONCERN_WORDS: &[&str] = &["may not cover", "needs attention", "short"];

/// Words a `concern` text must not use.
const REASSURANCE_WORDS: &[&str] = &["on track", "expected and", "covered", "less than usual"];

/// The longest feed text (Appendix B.3).
pub const MAX_FEED_LEN: usize = 140;

/// Why a text failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A banned word or phrase.
    Banned(&'static str),
    /// A word of the other tone.
    WrongTone(&'static str),
    /// Longer than [`MAX_FEED_LEN`].
    TooLong(usize),
    /// An exclamation mark, emoji, or markdown.
    Format(char),
    /// Empty.
    Empty,
}

fn contains_phrase(text: &str, phrase: &str) -> bool {
    let lower = text.to_lowercase();
    let mut start = 0;
    while let Some(i) = lower[start..].find(phrase) {
        let at = start + i;
        let before = lower[..at].chars().next_back();
        let after = lower[at + phrase.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
        if boundary(before) && boundary(after) {
            return true;
        }
        start = at + phrase.len();
    }
    false
}

/// Checks one feed text. Returns every problem.
pub fn check(text: &str, kind: Kind) -> Vec<Problem> {
    let mut problems = Vec::new();
    if text.trim().is_empty() {
        problems.push(Problem::Empty);
    }
    let len = text.chars().count();
    if len > MAX_FEED_LEN {
        problems.push(Problem::TooLong(len));
    }
    for c in text.chars() {
        if matches!(c, '!' | '*' | '_' | '#' | '`') || u32::from(c) >= 0x1F000 {
            problems.push(Problem::Format(c));
        }
    }
    problems.extend(
        BANNED
            .iter()
            .filter(|w| contains_phrase(text, w))
            .map(|w| Problem::Banned(w)),
    );
    let other: &[&'static str] = match kind {
        Kind::Reassurance => CONCERN_WORDS,
        Kind::Concern => REASSURANCE_WORDS,
        _ => &[],
    };
    problems.extend(
        other
            .iter()
            .filter(|w| contains_phrase(text, w))
            .map(|w| Problem::WrongTone(w)),
    );
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catches_banned_words_as_whole_words() {
        assert_eq!(
            check("You overspent on dining.", Kind::Awareness),
            [Problem::Banned("overspent")]
        );
        assert_eq!(
            check("Don't worry, it's Netflix.", Kind::Reassurance),
            [Problem::Banned("don't worry")]
        );
        // "badge" and "Mustang" are not "bad" and "must".
        assert_eq!(check("A badge from Mustang Coffee.", Kind::Awareness), []);
    }

    #[test]
    fn catches_format_length_and_tone() {
        assert_eq!(check("Great!", Kind::Reassurance), [Problem::Format('!')]);
        assert!(matches!(
            check(&"a".repeat(141), Kind::Awareness)[0],
            Problem::TooLong(141)
        ));
        assert_eq!(
            check("You are on track.", Kind::Concern),
            [Problem::WrongTone("on track")]
        );
        assert_eq!(
            check("Cash may not cover rent.", Kind::Reassurance),
            [Problem::WrongTone("may not cover")]
        );
        assert_eq!(check(" ", Kind::Awareness), [Problem::Empty]);
    }
}
