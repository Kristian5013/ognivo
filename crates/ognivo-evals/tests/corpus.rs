//! T0's parse corpus on every commit: the failure shapes of the recorded runs
//! (`evals/t0/corpus/*.sse`, neutral text in the layout llama-server streamed it, each
//! naming the recording it follows), read by the Generic driver's own code and named as
//! the replay names them. Cut anywhere before its end, a stream allows nothing.

use std::path::Path;

use ognivo_evals::replay::verdict;
use ognivo_models::{Guard, profile};

#[test]
fn recorded_failure_shapes_get_their_verdicts() {
    let profiles = profile::builtin().unwrap();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/t0/corpus");
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sse"))
        .collect();
    files.sort();
    assert!(files.len() >= 11, "{files:?}");
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let body = std::fs::read_to_string(&path).unwrap();
        let field = |key: &str| {
            body.lines()
                .find_map(|l| l.strip_prefix(&format!(": {key} ")))
                .unwrap_or_else(|| panic!("{name}: no `: {key}` line"))
        };
        let p = profiles.iter().find(|p| p.id == field("profile")).unwrap();
        assert!(
            p.tokens.guarded().all(|t| !body.contains(t)),
            "{name} holds a control token in one piece (§5.3)"
        );
        let guard = Guard::new(&p.tokens);
        assert_eq!(verdict(200, &body, &guard), field("verdict"), "{name}");

        // every cut at a line start or inside a line, up to the end of `[DONE]`
        let end = body.find("data: [DONE]").map_or(body.len(), |i| i + 12);
        let mut cuts: Vec<usize> = body[..end]
            .match_indices('\n')
            .map(|(i, _)| i + 1)
            .collect();
        cuts.extend(cuts.clone().iter().map(|i| i + 7).filter(|&i| i < end));
        cuts.push(end);
        for cut in cuts {
            if cut == body.len() || !body.is_char_boundary(cut) {
                continue;
            }
            let v = verdict(200, &body[..cut], &guard);
            assert!(
                v == "incomplete" || v.starts_with("repeating:") || v.starts_with("error:"),
                "{name} cut at {cut}: {v}"
            );
        }
    }
}
