use std::fs;
use tscodeselection::search::{SearchMode, walk, walk_with_excludes};

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

#[test]
fn configured_broad_excludes_prune_named_components_and_relative_subtrees() {
    let temp = tempfile::tempdir().unwrap();
    for relative in [
        "keep.txt",
        "vendor/root.txt",
        "nested/vendor/dependency.txt",
        "generated/cache/data.txt",
        "other/generated/data.txt",
    ] {
        let path = temp.path().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, relative).unwrap();
    }

    let files = walk_with_excludes(
        temp.path(),
        SearchMode::Broad,
        &["vendor".into(), "generated/cache".into()],
    );
    assert!(files.iter().any(|path| path.ends_with("keep.txt")));
    assert!(
        files
            .iter()
            .any(|path| path.ends_with("other/generated/data.txt"))
    );
    assert!(!files.iter().any(|path| path.ends_with("vendor/root.txt")));
    assert!(!files.iter().any(|path| path.ends_with("dependency.txt")));
    assert!(
        !files
            .iter()
            .any(|path| path.ends_with("generated/cache/data.txt"))
    );
}

#[cfg(unix)]
#[test]
fn repository_walk_does_not_follow_file_or_directory_symlinks() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "secret").unwrap();
    symlink(
        outside.path().join("secret.txt"),
        root.path().join("linked-secret.txt"),
    )
    .unwrap();
    symlink(outside.path(), root.path().join("linked-directory")).unwrap();

    for mode in [SearchMode::GitAware, SearchMode::Broad] {
        let files = walk(root.path(), mode);
        assert!(!files.iter().any(|path| path.ends_with("linked-secret.txt")));
        assert!(!files.iter().any(|path| path.ends_with("secret.txt")));
    }
}
