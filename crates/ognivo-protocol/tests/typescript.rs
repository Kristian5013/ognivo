//! The TypeScript of the event model and of the core's methods, which the GUI imports
//! (`apps/ui/src/protocol.ts`), is what the Rust types make of it:
//! `OGNIVO_BLESS=1 cargo test -p ognivo-protocol` writes it again after a change.

use std::path::Path;

use ognivo_protocol::rpc::{self, *};
use ognivo_protocol::*;
use ts_rs::{Config, TS};

/// A method's name, its params and its result, as TypeScript names them.
fn method<P: TS, R: TS>(cfg: &Config, name: &str) -> String {
    format!(
        "  {name:?}: {{ params: {}; result: {} }};\n",
        P::name(cfg),
        R::name(cfg)
    )
}

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
        Initialize::decl(&cfg),
        Initialized::decl(&cfg),
        ListThreads::decl(&cfg),
        Threads::decl(&cfg),
        ThreadSummary::decl(&cfg),
        Context::decl(&cfg),
        Server::decl(&cfg),
        StartThread::decl(&cfg),
        ResumeThread::decl(&cfg),
        Opened::decl(&cfg),
        ThreadRef::decl(&cfg),
        ReadThread::decl(&cfg),
        ThreadEvents::decl(&cfg),
        Trajectory::decl(&cfg),
        StartTurn::decl(&cfg),
        EditTurn::decl(&cfg),
        Changes::decl(&cfg),
        ChangedFile::decl(&cfg),
        Accept::decl(&cfg),
        Accepted::decl(&cfg),
        Revert::decl(&cfg),
        Reverted::decl(&cfg),
        Done::decl(&cfg),
        ItemRef::decl(&cfg),
        ApprovalAsked::decl(&cfg),
        Decision::decl(&cfg),
        ApprovalAnswer::decl(&cfg),
        TrustAsked::decl(&cfg),
        Rule::decl(&cfg),
        TrustAnswer::decl(&cfg),
        McpTrustAsked::decl(&cfg),
        McpServer::decl(&cfg),
        McpSignInAsked::decl(&cfg),
        SignInAnswer::decl(&cfg),
        Detected::decl(&cfg),
        RoutedModel::decl(&cfg),
        McpServers::decl(&cfg),
        McpServerState::decl(&cfg),
        McpToolInfo::decl(&cfg),
        McpPromptInfo::decl(&cfg),
        McpPromptArgument::decl(&cfg),
        ProjectRef::decl(&cfg),
        McpConfig::decl(&cfg),
        McpConfigFile::decl(&cfg),
        McpEntry::decl(&cfg),
        McpConfigSet::decl(&cfg),
        McpServerRef::decl(&cfg),
        HooksTrustAsked::decl(&cfg),
        HookShown::decl(&cfg),
        Cancel::decl(&cfg),
    ];
    // what a client may call, and what the core may ask it
    let methods = [
        method::<Server, Detected>(&cfg, rpc::SERVER_DETECT),
        method::<ThreadRef, McpServers>(&cfg, rpc::MCP_SERVERS),
        method::<ProjectRef, McpConfig>(&cfg, rpc::MCP_CONFIG),
        method::<McpConfigSet, Done>(&cfg, rpc::MCP_CONFIG_SET),
        method::<McpServerRef, Done>(&cfg, rpc::MCP_LOGIN),
        method::<McpServerRef, Done>(&cfg, rpc::MCP_LOGOUT),
        method::<Initialize, Initialized>(&cfg, rpc::INITIALIZE),
        method::<ListThreads, Threads>(&cfg, rpc::THREAD_LIST),
        method::<StartThread, Opened>(&cfg, rpc::THREAD_START),
        method::<ResumeThread, Opened>(&cfg, rpc::THREAD_RESUME),
        method::<ReadThread, ThreadEvents>(&cfg, rpc::THREAD_READ),
        method::<ThreadRef, Trajectory>(&cfg, rpc::THREAD_TRAJECTORY),
        method::<ThreadRef, Done>(&cfg, rpc::THREAD_CLEAR),
        method::<ThreadRef, Done>(&cfg, rpc::THREAD_CLOSE),
        method::<ThreadRef, Done>(&cfg, rpc::THREAD_DELETE),
        method::<StartTurn, Done>(&cfg, rpc::TURN_START),
        method::<ThreadRef, Done>(&cfg, rpc::TURN_INTERRUPT),
        method::<ThreadRef, Done>(&cfg, rpc::TURN_ANSWER_NOW),
        method::<EditTurn, Done>(&cfg, rpc::TURN_EDIT),
        method::<ThreadRef, Changes>(&cfg, rpc::CHANGES_LIST),
        method::<Accept, Accepted>(&cfg, rpc::CHANGES_ACCEPT),
        method::<Revert, Reverted>(&cfg, rpc::CHANGES_REVERT),
    ];
    let asks = [
        method::<ApprovalAsked, ApprovalAnswer>(&cfg, rpc::APPROVAL_ASK),
        method::<TrustAsked, TrustAnswer>(&cfg, rpc::RULES_TRUST),
        method::<McpTrustAsked, TrustAnswer>(&cfg, rpc::MCP_TRUST),
        method::<McpSignInAsked, SignInAnswer>(&cfg, rpc::MCP_SIGN_IN),
        method::<HooksTrustAsked, TrustAnswer>(&cfg, rpc::HOOKS_TRUST),
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
    out.push_str("\n/** The core's methods. */\nexport type Methods = {\n");
    out.extend(methods);
    out.push_str("};\n\n/** What the core asks its clients. */\nexport type Asks = {\n");
    out.extend(asks);
    out.push_str(&format!(
        "}};\n\nexport const NOTIFY_EVENT = {:?};\nexport const NOTIFY_CANCEL = {:?};\n",
        rpc::EVENT,
        rpc::CANCEL
    ));
    out.push_str(&format!(
        "\nexport const ERRORS = {{ noThread: {}, busy: {}, notStarted: {}, journal: {}, unsupported: {}, noTurn: {} }} as const;\n",
        rpc::error::NO_THREAD,
        rpc::error::BUSY,
        rpc::error::NOT_STARTED,
        rpc::error::JOURNAL,
        rpc::error::UNSUPPORTED,
        rpc::error::NO_TURN
    ));
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
