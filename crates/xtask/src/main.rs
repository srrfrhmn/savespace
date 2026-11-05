//! `xtask`: repository automation for SaveSpace.
//!
//! Commands:
//! - `cargo xtask check-deps`: enforce the crate dependency rule (design doc §8.3).

use std::collections::BTreeSet;
use std::process::ExitCode;

use anyhow::Context;
use cargo_metadata::MetadataCommand;

const USAGE: &str = "usage: cargo xtask <command>

commands:
  check-deps  enforce the crate dependency rule (design doc §8.3)";

/// A command function. Add new commands to [`command`].
type Command = fn() -> anyhow::Result<()>;

fn command(name: &str) -> Option<Command> {
    [("check-deps", check_deps as Command)]
        .into_iter()
        .find_map(|(n, f)| (n == name).then_some(f))
}

fn main() -> ExitCode {
    let name = std::env::args().nth(1).unwrap_or_default();
    let Some(run) = command(&name) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// The layer of a workspace crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Layer {
    Core,
    Platform,
    Domain,
    Aggregator,
    Binding,
    App,
    Tool,
}

fn classify(name: &str) -> Option<Layer> {
    match name {
        "savespace-core" => Some(Layer::Core),
        "savespace-core-wasm" | "savespace-core-ffi" => Some(Layer::Binding),
        "api" | "worker" => Some(Layer::App),
        "xtask" => Some(Layer::Tool),
        n if n.starts_with("platform-") => Some(Layer::Platform),
        n if n.starts_with("domain-") => Some(Layer::Domain),
        n if n.starts_with("aggregator-") => Some(Layer::Aggregator),
        _ => None,
    }
}

/// Returns `true` if a crate named `from` may depend on the crate named `to`.
///
/// The rule (design doc §8.3): `bin` → `domain-*` → `platform-*` →
/// `savespace-core`. A domain crate never depends on another domain crate.
/// Aggregator adapters may depend on `domain-sync` only.
fn allowed(from: &str, to: &str) -> bool {
    let (Some(f), Some(t)) = (classify(from), classify(to)) else {
        return false;
    };
    match f {
        Layer::Core | Layer::Tool => false,
        Layer::Platform | Layer::Binding => t == Layer::Core,
        Layer::Domain => matches!(t, Layer::Platform | Layer::Core),
        Layer::Aggregator => {
            matches!(t, Layer::Platform | Layer::Core)
                || (t == Layer::Domain && to == "domain-sync")
        }
        Layer::App => !matches!(t, Layer::App | Layer::Tool | Layer::Binding),
    }
}

fn check_deps() -> anyhow::Result<()> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("cargo metadata")?;
    let members: BTreeSet<&str> = metadata
        .workspace_packages()
        .iter()
        .map(|p| p.name.as_str())
        .collect();

    let mut violations = Vec::new();
    for package in metadata.workspace_packages() {
        let from = package.name.as_str();
        if classify(from).is_none() {
            violations.push(format!(
                "{from}: unknown crate layer; add it to `classify` in crates/xtask"
            ));
            continue;
        }
        for dep in &package.dependencies {
            let to = dep.name.as_str();
            if to == from || !members.contains(to) {
                continue;
            }
            if !allowed(from, to) {
                violations.push(format!("{from} → {to} ({:?} dependency)", dep.kind));
            }
        }
    }

    if violations.is_empty() {
        eprintln!(
            "check-deps: {} workspace crates follow the dependency rule",
            members.len()
        );
        Ok(())
    } else {
        for v in &violations {
            eprintln!("  forbidden: {v}");
        }
        anyhow::bail!(
            "{} crate dependency rule violation(s). See design doc §8.3.",
            violations.len()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::allowed;

    #[test]
    fn layering() {
        assert!(allowed("domain-ledger", "platform-db"));
        assert!(allowed("domain-ledger", "savespace-core"));
        assert!(allowed("platform-db", "savespace-core"));
        assert!(allowed("aggregator-plaid", "domain-sync"));
        assert!(allowed("api", "domain-ledger"));
        assert!(allowed("savespace-core-wasm", "savespace-core"));
    }

    #[test]
    fn forbidden() {
        assert!(!allowed("domain-ledger", "domain-sync"));
        assert!(!allowed("savespace-core", "platform-db"));
        assert!(!allowed("platform-db", "domain-ledger"));
        assert!(!allowed("aggregator-plaid", "domain-ledger"));
        assert!(!allowed("savespace-core-wasm", "platform-db"));
        assert!(!allowed("domain-ledger", "api"));
        assert!(!allowed("api", "worker"));
        assert!(!allowed("unknown", "savespace-core"));
    }
}
