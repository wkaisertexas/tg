use std::fs;
use tscodeselection::search::{SearchMode, walk};

#[test]
fn broad_search_includes_ignored_files_but_never_git_metadata() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir(temp.path().join(".git")).unwrap();
    fs::write(temp.path().join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir(temp.path().join("ignored")).unwrap();
    fs::write(
        temp.path().join("ignored/generated.cpp"),
        "struct Fixture {};",
    )
    .unwrap();
    fs::write(temp.path().join("visible.rs"), "struct Visible;").unwrap();
    fs::write(temp.path().join(".git/config"), "private").unwrap();

    let ordinary = walk(temp.path(), SearchMode::GitAware);
    let broad = walk(temp.path(), SearchMode::Broad);
    assert!(ordinary.iter().any(|path| path.ends_with("visible.rs")));
    assert!(!ordinary.iter().any(|path| path.ends_with("generated.cpp")));
    assert!(broad.iter().any(|path| path.ends_with("generated.cpp")));
    assert!(!broad.iter().any(|path| path.ends_with("config")));
}
