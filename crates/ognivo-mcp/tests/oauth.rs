//! Signing in to a remote server end to end, against a script of these tests that is both
//! an MCP server behind OAuth and its authorization server: the gateway sees that the
//! server wants a sign-in, the sign-in registers the client, goes through the "browser"
//! (the test, following the redirect) and keeps the token for the user alone; the next
//! start uses it and refreshes it when it runs out; a sign-out forgets it. Python 3,
//! nothing beyond the loopback.

use std::collections::BTreeMap;
use std::io::BufRead as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use ognivo_mcp::{Gateway, ServerConfig, Transport, oauth};

/// The script, with tokens of `lifetime` seconds, and its URL.
fn server(lifetime: u32) -> (Child, String) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oauth_server.py");
    let mut child = Command::new("python3")
        .arg("-I")
        .arg(&script)
        .args(["--lifetime", &lifetime.to_string()])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    (child, format!("http://127.0.0.1:{}/mcp", port.trim()))
}

fn config(url: &str) -> ServerConfig {
    ServerConfig {
        name: "docs".into(),
        transport: Transport::Http {
            url: url.into(),
            headers: BTreeMap::new(),
            oauth: Default::default(),
        },
        source: "test".into(),
    }
}

/// The user's browser: it opens the page and, the user saying yes, comes back.
fn browser(page: &str) {
    let page = page.to_owned();
    tokio::spawn(async move {
        let c = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let r = c.get(&page).send().await.unwrap();
        assert_eq!(r.status(), 302, "{page}");
        let back = r.headers()["location"].to_str().unwrap().to_owned();
        assert!(back.starts_with("http://127.0.0.1:"), "{back}");
        let r = c.get(&back).send().await.unwrap();
        assert!(
            r.text()
                .await
                .unwrap()
                .contains("you are signed in to docs")
        );
    });
}

const START: Duration = Duration::from_secs(20);
const CALL: Duration = Duration::from_secs(10);

#[tokio::test]
async fn a_sign_in_is_asked_for_kept_used_and_forgotten() {
    use std::os::unix::fs::PermissionsExt as _;
    let (mut child, url) = server(3600);
    let home = tempfile::tempdir().unwrap();
    let dir = oauth::credentials_dir(home.path());
    let docs = config(&url);

    let g = Gateway::start(std::slice::from_ref(&docs), START, Some(&dir)).await;
    let st = &g.started()[0];
    assert_eq!(
        st.outcome,
        Err("it wants you to sign in: ognivo mcp login docs".into())
    );
    let challenge = st.sign_in.clone().unwrap();
    assert!(challenge.contains("resource_metadata="), "{challenge}");
    g.shutdown().await;
    assert!(!oauth::signed_in(&dir, &docs));

    oauth::sign_in(&dir, &docs, Some(&challenge), browser)
        .await
        .unwrap();
    assert!(oauth::signed_in(&dir, &docs));
    let file = oauth::credentials_file(&dir, "docs");
    let mode = std::fs::metadata(&file).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let kept = std::fs::read_to_string(&file).unwrap();
    assert!(kept.contains(&url) && kept.contains("tok-1"), "{kept}");

    let g = Gateway::start(std::slice::from_ref(&docs), START, Some(&dir)).await;
    assert_eq!(g.started()[0].outcome, Ok(1), "{:?}", g.started());
    assert_eq!(
        g.call("mcp__docs__whoami", "{}", CALL).await,
        Ok("[mcp: docs · whoami]\nsigned in with tok-1".into())
    );
    g.shutdown().await;
    // a gateway not given the sign-ins does not use them
    let g = Gateway::start(std::slice::from_ref(&docs), START, None).await;
    assert!(g.started()[0].sign_in.is_some());
    g.shutdown().await;

    assert_eq!(oauth::sign_out(&dir, &docs), Ok(true));
    assert_eq!(oauth::sign_out(&dir, &docs), Ok(false));
    let g = Gateway::start(std::slice::from_ref(&docs), START, Some(&dir)).await;
    assert!(g.started()[0].sign_in.is_some(), "{:?}", g.started());
    g.shutdown().await;
    child.kill().unwrap();
    child.wait().unwrap();
}

#[tokio::test]
async fn a_token_that_runs_out_is_refreshed_and_the_new_one_kept() {
    // tokens of 20 s are within the client's 30 s to refresh at once
    let (mut child, url) = server(20);
    let home = tempfile::tempdir().unwrap();
    let dir = oauth::credentials_dir(home.path());
    let docs = config(&url);
    oauth::sign_in(&dir, &docs, None, browser).await.unwrap();
    let file = oauth::credentials_file(&dir, "docs");
    assert!(std::fs::read_to_string(&file).unwrap().contains("tok-1"));

    let g = Gateway::start(std::slice::from_ref(&docs), START, Some(&dir)).await;
    assert_eq!(g.started()[0].outcome, Ok(1), "{:?}", g.started());
    let said = g.call("mcp__docs__whoami", "{}", CALL).await.unwrap();
    assert!(
        said.starts_with("[mcp: docs · whoami]\nsigned in with tok-") && !said.ends_with("tok-1"),
        "{said}"
    );
    g.shutdown().await;
    // the refreshed token is the one kept, and the spent refresh token is gone with it
    let kept = std::fs::read_to_string(&file).unwrap();
    assert!(
        !kept.contains("\"tok-1\"") && !kept.contains("\"ref-1\""),
        "{kept}"
    );
    child.kill().unwrap();
    child.wait().unwrap();
}
