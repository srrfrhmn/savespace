//! The view policy (design doc §7.1, §7.2 steps 4–5).
//!
//! - Every `concern` shows, and each one takes a slot.
//! - The other kinds fill the slots left of 3, with a limit per kind.
//! - Conflict rule: a `reassurance` does not show if a `concern` covers the
//!   same scope.
//! - Within the limits, higher relevance shows first.

use crate::model::Kind;

/// The most insights in one view, besides extra concerns.
pub const MAX_PER_VIEW: usize = 3;

/// What the policy needs to know about an insight.
pub trait Shown {
    /// The kind.
    fn kind(&self) -> Kind;
    /// The scope.
    fn scope(&self) -> &str;
    /// 0 to 10,000.
    fn relevance_bp(&self) -> i32;
}

impl Shown for crate::model::Candidate {
    fn kind(&self) -> Kind {
        self.kind
    }
    fn scope(&self) -> &str {
        &self.scope
    }
    fn relevance_bp(&self) -> i32 {
        self.relevance_bp
    }
}

/// Picks the insights for one view, in display order: concerns first, then
/// by relevance.
pub fn select<T: Shown>(mut items: Vec<T>) -> Vec<T> {
    items.sort_by(|a, b| {
        (a.kind() != Kind::Concern)
            .cmp(&(b.kind() != Kind::Concern))
            .then(b.relevance_bp().cmp(&a.relevance_bp()))
    });
    let concern_scopes: Vec<String> = items
        .iter()
        .filter(|i| i.kind() == Kind::Concern)
        .map(|i| i.scope().to_owned())
        .collect();
    let mut out = Vec::new();
    let mut per_kind: Vec<(Kind, usize)> = Vec::new();
    for item in items {
        let kind = item.kind();
        if kind == Kind::Concern {
            out.push(item);
            continue;
        }
        if kind == Kind::Reassurance && concern_scopes.iter().any(|s| s == item.scope()) {
            continue;
        }
        if out.len() >= MAX_PER_VIEW {
            continue;
        }
        let count = per_kind
            .iter()
            .find(|(k, _)| *k == kind)
            .map_or(0, |(_, n)| *n);
        if kind.max_per_view().is_some_and(|max| count >= max) {
            continue;
        }
        match per_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => per_kind.push((kind, 1)),
        }
        out.push(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct Item(Kind, &'static str, i32);

    impl Shown for Item {
        fn kind(&self) -> Kind {
            self.0
        }
        fn scope(&self) -> &str {
            self.1
        }
        fn relevance_bp(&self) -> i32 {
            self.2
        }
    }

    use Kind::{Anticipation, Awareness, Concern, Reassurance};

    #[test]
    fn at_most_three_with_limits_per_kind() {
        let picked = select(vec![
            Item(Reassurance, "a", 5000),
            Item(Reassurance, "b", 6000),
            Item(Reassurance, "c", 7000),
            Item(Awareness, "d", 1000),
            Item(Awareness, "e", 2000),
            Item(Anticipation, "f", 100),
        ]);
        assert_eq!(
            picked,
            [
                Item(Reassurance, "c", 7000),
                Item(Reassurance, "b", 6000),
                Item(Awareness, "e", 2000)
            ]
        );
    }

    #[test]
    fn concerns_always_show_and_take_slots() {
        let picked = select(vec![
            Item(Anticipation, "x", 9000),
            Item(Concern, "m", 10_000),
            Item(Concern, "n", 10_000),
            Item(Concern, "o", 10_000),
            Item(Concern, "p", 10_000),
        ]);
        assert_eq!(picked.len(), 4);
        assert!(picked.iter().all(|i| i.0 == Concern));
    }

    #[test]
    fn no_reassurance_under_a_concern_for_the_same_scope() {
        let picked = select(vec![
            Item(Reassurance, "month:2026-11", 9000),
            Item(Reassurance, "week:2026-11-16", 6000),
            Item(Concern, "month:2026-11", 10_000),
        ]);
        assert_eq!(
            picked,
            [
                Item(Concern, "month:2026-11", 10_000),
                Item(Reassurance, "week:2026-11-16", 6000)
            ]
        );
    }
}
