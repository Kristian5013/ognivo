//! The TypeScript of the event model, which the GUI imports (`apps/ui/src/protocol.ts`), is
//! what the Rust types make of it: `OGNIVO_BLESS=1 cargo test -p ognivo-protocol` writes it
//! again after a change.

use std::path::Path;

use ognivo_protocol::*;
use ts_rs::{Config, TS};

fn typescript() -> String {
    let cfg = Config::new().with_large_int("number");
    let decls = [
        Event::decl(&cfg),
        EventKind::decl(&cfg),
        ThreadId::decl(&cfg),
        TurnId::decl(&cfg),
        ItemId::decl(&cfg),
        ThreadInfo::decl(&cfg),
        Parent::decl(&cfg),
        Item::decl(&cfg),
        ItemBody::decl(&cfg),
        Call::decl(&cfg),
        Task::decl(&cfg),
        ToolResult::decl(&cfg),
        Outcome::decl(&cfg),
        FileChange::decl(&cfg),
        Hunk::decl(&cfg),
        VoidReason::decl(&cfg),
        RawCall::decl(&cfg),
        Skipped::decl(&cfg),
        Generation::decl(&cfg),
        Delta::decl(&cfg),
        TurnEnd::decl(&cfg),
    ];
    let mut out = String::from(
        "// The event model of ognivo, generated from crates/ognivo-protocol: edit the Rust types,\n// then `OGNIVO_BLESS=1 cargo test -p ognivo-protocol` writes this again.\n\n",
    );
    out.push_str(&format!("export const VERSION = {VERSION};\n"));
    for d in decls {
        out.push_str("\nexport ");
        out.push_str(&d);
        out.push('\n');
    }
    out
}

#[test]
fn the_gui_has_the_event_model_as_the_core_sends_it() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../apps/ui/src/protocol.ts");
    let want = typescript();
    if std::env::var_os("OGNIVO_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is not what the Rust types make of it: run OGNIVO_BLESS=1 cargo test -p ognivo-protocol",
        path.display()
    );
}
