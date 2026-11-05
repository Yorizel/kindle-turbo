//! Runs the real binary.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]

mod common;

use common::{Chapter, EpubSpec, build_epub, prose};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_kindle-turbo");

fn sample_epub(dir: &std::path::Path, title: &str) -> std::path::PathBuf {
    let mut spec = EpubSpec { title: title.into(), ..EpubSpec::default() };
    spec.chapters.push(Chapter { file: "a.xhtml".into(), title: "A".into(), body: prose("a", 50) });
    spec.toc.push(("A".into(), "a.xhtml".into(), vec![]));
    let p = dir.join(format!("{title}.epub"));
    std::fs::write(&p, build_epub(&spec)).unwrap();
    p
}

#[test]
fn converts_many_files_and_checks_them() {
    let dir = tempfile::tempdir().unwrap();
    let inputs: Vec<_> = (0..6).map(|i| sample_epub(dir.path(), &format!("Livro {i}"))).collect();
    let out = dir.path().join("out");
    let st = Command::new(BIN).arg("--offline").arg("-o").arg(&out).args(&inputs).output().unwrap();
    assert!(st.status.success(), "{}", String::from_utf8_lossy(&st.stderr));
    let produced: Vec<_> = (0..6).map(|i| out.join(format!("Livro {i}.azw3"))).collect();
    for p in &produced {
        assert!(p.exists(), "{} missing", p.display());
    }
    let chk = Command::new(BIN).arg("--check").args(&produced).output().unwrap();
    assert!(chk.status.success());
    assert_eq!(String::from_utf8_lossy(&chk.stdout).matches("ok ").count(), 6);
}

#[test]
fn check_rejects_garbage() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("x.azw3");
    std::fs::write(&p, b"garbage").unwrap();
    let chk = Command::new(BIN).arg("--check").arg(&p).output().unwrap();
    assert!(!chk.status.success());
}

#[test]
fn bad_input_reports_error_but_converts_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let good = sample_epub(dir.path(), "Bom");
    let bad = dir.path().join("ruim.epub");
    std::fs::write(&bad, b"nope").unwrap();
    let out = dir.path().join("o");
    let st = Command::new(BIN).arg("--offline").arg("-o").arg(&out).arg(&good).arg(&bad).output().unwrap();
    assert!(!st.status.success());
    assert!(out.join("Bom.azw3").exists());
    assert!(String::from_utf8_lossy(&st.stdout).contains("ERRO"));
}

#[test]
fn help_works() {
    let st = Command::new(BIN).arg("--help").output().unwrap();
    assert!(st.status.success());
    assert!(String::from_utf8_lossy(&st.stdout).contains("--kindle"));
}
