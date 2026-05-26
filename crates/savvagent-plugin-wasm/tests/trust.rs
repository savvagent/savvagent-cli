//! Integration tests for the public trust API — round-trip `TrustFile`
//! through disk and exercise the `tree_hash` invariants Tasks 6+ depend on.

use savvagent_plugin_wasm::trust::{TrustCheck, TrustFile, tree_hash};

#[test]
fn round_trip_through_disk() {
    let tmp = tempfile::tempdir().unwrap();
    let mut tf = TrustFile::default();
    tf.trust("acme.demo", "abc".into(), None);
    tf.save(tmp.path()).unwrap();
    let loaded = TrustFile::load(tmp.path()).unwrap();
    assert_eq!(loaded.check("acme.demo", "abc"), TrustCheck::Ok);
}

#[test]
fn missing_trust_file_returns_default() {
    let tmp = tempfile::tempdir().unwrap();
    let tf = TrustFile::load(tmp.path()).unwrap();
    assert!(tf.plugins.is_empty());
}

#[test]
fn tree_hash_includes_filenames() {
    // Two files with swapped contents must hash differently from two
    // files with the same contents but swapped *names* — i.e. file
    // identity (name + content) is what the digest commits to, not just
    // the byte multiset of file contents.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a"), b"X").unwrap();
    std::fs::write(tmp.path().join("b"), b"Y").unwrap();
    let h1 = tree_hash(tmp.path()).unwrap();

    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::write(tmp2.path().join("a"), b"Y").unwrap();
    std::fs::write(tmp2.path().join("b"), b"X").unwrap();
    let h2 = tree_hash(tmp2.path()).unwrap();
    assert_ne!(h1, h2, "filename<->content pairing must affect hash");
}

#[test]
fn save_creates_intermediate_dirs() {
    // `home_dir` may not yet have a `.savvagent/` subdirectory; `save`
    // must create it on the way in.
    let tmp = tempfile::tempdir().unwrap();
    let mut tf = TrustFile::default();
    tf.trust("acme.demo", "abc".into(), None);
    tf.save(tmp.path()).unwrap();
    assert!(tmp.path().join(".savvagent/plugin-trust.toml").is_file());
}

#[test]
fn revoke_then_reload_is_untrusted() {
    let tmp = tempfile::tempdir().unwrap();
    let mut tf = TrustFile::default();
    tf.trust("acme.demo", "abc".into(), None);
    tf.save(tmp.path()).unwrap();
    let mut loaded = TrustFile::load(tmp.path()).unwrap();
    loaded.revoke("acme.demo");
    loaded.save(tmp.path()).unwrap();
    let re = TrustFile::load(tmp.path()).unwrap();
    assert_eq!(re.check("acme.demo", "abc"), TrustCheck::Untrusted);
}
