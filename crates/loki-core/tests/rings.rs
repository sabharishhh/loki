//! The ring dependency rule, checked rather than trusted.
//!
//! Ring 0 (`core`) and Ring 1 (`ports`) must not reach into Ring 2 (`adapters`). If they do, the
//! core knows about a particular provider and swapping one stops being free, which is the whole
//! point of the architecture.
//!
//! A doc comment saying so does not survive a hurried afternoon. A failing test does.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(rust_files(&path));
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
    found
}

/// Lines of real code, with `//` comments and blank lines dropped.
///
/// Without this the rule would fire on a doc comment that merely names an adapter, which is how a
/// check like this ends up disabled.
fn code_lines(path: &Path) -> Vec<(usize, String)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .enumerate()
        .map(|(n, line)| (n + 1, line.trim().to_owned()))
        .filter(|(_, line)| !line.is_empty() && !line.starts_with("//"))
        .collect()
}

fn src(module: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join(module)
}

#[test]
fn ring_zero_and_one_do_not_reach_into_ring_two() {
    let mut offences = Vec::new();

    // `memory` is Ring 0 too: section 6.2 lists the typestate gate among the locked internals.
    // It was outside this check until an audit noticed, which is how a rule quietly stops holding.
    for module in ["core", "ports", "memory"] {
        for file in rust_files(&src(module)) {
            for (number, line) in code_lines(&file) {
                let reaches = line.contains("crate::adapters")
                    || line.contains("super::adapters")
                    || line.contains("use crate::adapters");
                if reaches {
                    let name = file
                        .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&file)
                        .display();
                    offences.push(format!("  {name}:{number}  {line}"));
                }
            }
        }
    }

    assert!(
        offences.is_empty(),
        "Ring 0 or Ring 1 depends on Ring 2. Route it through a port instead.\n{}",
        offences.join("\n")
    );
}

#[test]
fn ring_two_never_talks_to_ring_two() {
    let adapters = src("adapters");
    let mut offences = Vec::new();

    // Modules an adapter may share. `sse` and `pricing` are common infrastructure, not providers,
    // so they are not another adapter in the sense the rule cares about. `*` is the glob a test
    // submodule uses to reach its own file.
    let shared = ["sse", "pricing", "mod", "*"];

    for file in rust_files(&adapters) {
        let own = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        for (number, line) in code_lines(&file) {
            let Some(rest) = line.strip_prefix("use super::") else {
                continue;
            };
            let target = rest.split([':', ';', '{', ' ']).next().unwrap_or("").trim();
            if target.is_empty() || target == own || shared.contains(&target) {
                continue;
            }
            let name = file
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap_or(&file)
                .display();
            offences.push(format!("  {name}:{number}  {line}"));
        }
    }

    assert!(
        offences.is_empty(),
        "One adapter depends on another. Everything routes through Ring 1.\n{}",
        offences.join("\n")
    );
}

#[test]
fn the_check_can_actually_see_the_source() {
    assert!(
        rust_files(&src("core")).len() > 5,
        "found almost no source files, so the ring checks prove nothing"
    );
    assert!(!rust_files(&src("adapters")).is_empty());
    assert!(
        !rust_files(&src("memory")).is_empty(),
        "memory is Ring 0 and must be covered"
    );
}

/// §21.7: one egress point, and a test that says so.
///
/// **A promise nobody has exercised is a guess.** §9.11's locality tier is enforced by the type
/// system on the paths the compiler can see and by nothing at all on a path somebody adds later.
/// `adapters/egress.rs` emits an event before every send, so a second client anywhere in the tree
/// is traffic the event stream cannot describe: failure point 88, which is the shape the Grok
/// Build capture found in somebody else's product.
///
/// The list is deliberately literal rather than clever. A check nobody can read is a check
/// somebody deletes.
#[test]
fn nothing_but_the_egress_adapter_builds_a_transport() {
    const TRANSPORTS: [&str; 8] = [
        "reqwest::Client",
        "reqwest::get",
        "reqwest::blocking",
        // A socket is a socket. The delegated exit binds a listener and dials upstream, and a
        // second one of either anywhere else is the browser-shaped hole in §21.7's accounting.
        "TcpListener::bind",
        "TcpStream::connect",
        "TcpStream::connect",
        "hyper::Client",
        "ureq::",
    ];

    let mut offences = Vec::new();
    for module in ["core", "ports", "memory", "adapters"] {
        for file in rust_files(&src(module)) {
            // The one place allowed to. `ports/egress.rs` is deliberately not exempt: Ring 1
            // naming a transport is the other half of the same rule.
            if file.ends_with("adapters/egress.rs") {
                continue;
            }
            for (number, line) in code_lines(&file) {
                for transport in TRANSPORTS {
                    if line.contains(transport) {
                        offences.push(format!(
                            "{}:{number} builds a transport: {line}",
                            file.display()
                        ));
                    }
                }
            }
        }
    }

    assert!(
        offences.is_empty(),
        "every outbound request leaves through ports::egress (§21.7):\n{}",
        offences.join("\n")
    );
}
