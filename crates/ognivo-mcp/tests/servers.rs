//! The gateway against servers of the initialize era (scripts of the tests), over stdio and
//! over Streamable HTTP: they start, list, call, fail as the server says, and end. Python 3,
//! nothing beyond the loopback.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use ognivo_mcp::{Gateway, ServerConfig, Transport};

fn echo() -> ServerConfig {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/echo.py");
    ServerConfig {
        name: "echo".into(),
        transport: Transport::Stdio {
            command: "python3".into(),
            args: vec!["-I".into(), script.display().to_string()],
            env: BTreeMap::new(),
        },
        source: "test".into(),
    }
}

#[tokio::test]
async fn a_server_of_the_initialize_era_starts_lists_and_answers() {
    let missing = ServerConfig {
        name: "gone".into(),
        transport: Transport::Stdio {
            command: "/nonexistent/mcp-server".into(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
        source: "test".into(),
    };
    let g = Gateway::start(&[echo(), missing], Duration::from_secs(20), None).await;
    let started = g.started();
    assert_eq!(started[0].outcome, Ok(3), "{started:?}");
    assert!(
        matches!(&started[1].outcome, Err(why) if why.starts_with("/nonexistent/mcp-server")),
        "{started:?}"
    );
    let names: Vec<&str> = g.tools().iter().map(|t| t.name.as_str()).collect();
    assert_eq!(
        names,
        ["mcp__echo__add_up", "mcp__echo__echo", "mcp__echo__fail"]
    );
    let echo = g.tool("mcp__echo__echo").unwrap();
    assert!(echo.read_only);
    assert_eq!(echo.schema["required"][0], "text");

    let t = Duration::from_secs(10);
    assert_eq!(
        g.call("mcp__echo__echo", r#"{"text": "hello"}"#, t).await,
        Ok("[mcp: echo · echo]\nhello".into())
    );
    // the server's own name goes to it
    assert_eq!(
        g.call("mcp__echo__add_up", r#"{"numbers": [1, 2, 3.5]}"#, t)
            .await,
        Ok("[mcp: echo · add-up]\n{\n  \"sum\": 6.5\n}".into())
    );
    assert_eq!(
        g.call("mcp__echo__fail", "{}", t).await,
        Ok("[error] echo · fail failed:\nit broke".into())
    );
    assert!(g.call("mcp__echo__echo", "[1]", t).await.is_err());
    assert!(g.call("mcp__echo__nope", "{}", t).await.is_err());
    g.shutdown().await;
}

#[tokio::test]
async fn a_server_over_streamable_http_gets_its_headers() {
    use std::io::BufRead as _;
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/http_echo.py");
    let mut child = std::process::Command::new("python3")
        .arg("-I")
        .arg(&script)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", port.trim());
    let config = |token: &str| ServerConfig {
        name: "web".into(),
        transport: Transport::Http {
            url: url.clone(),
            headers: [("Authorization".to_owned(), format!("Bearer {token}"))].into(),
            oauth: Default::default(),
        },
        source: "test".into(),
    };
    let g = Gateway::start(&[config("t0k")], Duration::from_secs(20), None).await;
    assert_eq!(g.started()[0].outcome, Ok(1), "{:?}", g.started());
    assert_eq!(
        g.call(
            "mcp__web__echo",
            r#"{"text": "hi"}"#,
            Duration::from_secs(10)
        )
        .await,
        Ok("[mcp: web · echo]\nover http: hi".into())
    );
    g.shutdown().await;
    // without the token the server says no
    let g = Gateway::start(&[config("wrong")], Duration::from_secs(20), None).await;
    assert!(g.started()[0].outcome.is_err(), "{:?}", g.started());
    child.kill().unwrap();
    child.wait().unwrap();
}
