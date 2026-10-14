use super::parse;
use crate::support::TempTree;
use std::fs;
use token_tracker::adapters::codex::{CodexSessionDiscovery, CodexSessionParser};
use token_tracker::adapters::files::FileSessionSource;
use token_tracker::application::SessionSource;

const ROLLOUT: &str = include_str!("../../fixtures/codex/response-mirrors.jsonl");

#[test]
fn loading_resolves_format_changes_after_discovery() {
    let tree = TempTree::new();
    let plain = tree.write("rollout-session.jsonl", ROLLOUT);
    let compressed = plain.with_extension("jsonl.zst");
    let source = FileSessionSource::new(
        CodexSessionDiscovery::new([&tree.root]),
        CodexSessionParser::new(),
    );
    let original = source.discover(&[]).unwrap().sources.remove(0);
    fs::write(
        &compressed,
        zstd::stream::encode_all(ROLLOUT.as_bytes(), 0).unwrap(),
    )
    .unwrap();
    fs::remove_file(&plain).unwrap();
    let expected = parse(ROLLOUT).unwrap();
    assert_eq!(source.load(&original).unwrap().session, expected);

    let archived = source.discover(&[]).unwrap().sources.remove(0);
    assert_eq!(archived.key, original.key);
    fs::write(&plain, ROLLOUT).unwrap();
    fs::remove_file(&compressed).unwrap();
    assert_eq!(source.load(&archived).unwrap().session, expected);
}

#[test]
fn damaged_compressed_streams_fail_instead_of_importing_partial_usage() {
    let tree = TempTree::new();
    let source = FileSessionSource::new(
        CodexSessionDiscovery::new([&tree.root]),
        CodexSessionParser::new(),
    );
    let mut truncated = zstd::stream::encode_all(ROLLOUT.as_bytes(), 0).unwrap();
    truncated.pop();
    for bytes in [b"not a zstd stream".to_vec(), truncated] {
        tree.write("rollout-damaged.jsonl.zst", bytes);
        let discovered = source.discover(&[]).unwrap().sources.remove(0);
        assert!(source.load(&discovered).is_err());
    }
}
