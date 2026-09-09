use super::*;

#[test]
fn github_enterprise_identity_repository_and_empty_lists_succeed_on_the_same_host() {
    let fixture = Fixture::new();
    let report = report(
        fixture
            .doctor()
            .env("GH_HOST", "github.com")
            .args(["--check", "github"]),
        0,
    );
    for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
        let github = provider(&report, kind);
        assert_eq!(github["state"], "ready");
        assert_eq!(github["target"], "https://ghe.corp.example/team/project");
        assert!(github["checked_at"].is_u64());
        assert!(lines(&github["details"]).contains("authenticated identity validated"));
        assert!(github["actions"].as_array().unwrap().is_empty());
    }
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 6);
    for (sequence, subject) in calls.chunks(3).zip(["issue", "pr"]) {
        assert!(
            sequence
                .iter()
                .all(|call| call[0] == fixture.repo.to_str().unwrap())
        );
        assert_eq!(sequence[0][1..], ["api", "user", "--hostname", GH_HOST]);
        assert_eq!(
            sequence[1][1..],
            [
                "repo",
                "view",
                "--json",
                "nameWithOwner,url",
                "--",
                GH_REPOSITORY
            ]
        );
        assert_eq!(
            sequence[2][1..],
            [
                subject,
                "list",
                "--search",
                "",
                "--limit",
                "1",
                "--json",
                "number,url"
            ]
        );
    }
    assert!(fixture.calls("jira").is_empty());
    assert!(fixture.calls("git").is_empty());
}

#[test]
fn github_cannot_use_a_public_empty_listing_as_authentication_proof() {
    for (mode, state) in [("anonymous", "failed"), ("auth", "auth_failed")] {
        let fixture = Fixture::new();
        fixture.mode("gh", mode);
        let report = report(fixture.doctor().args(["--check", "github"]), 1);
        for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
            assert_eq!(provider(&report, kind)["state"], state);
            assert_ne!(provider(&report, kind)["state"], "ready");
        }
        let calls = fixture.calls("gh");
        assert_eq!(calls.len(), 2);
        assert!(
            calls
                .iter()
                .all(|call| call[1..] == ["api", "user", "--hostname", GH_HOST])
        );
        assert!(fixture.calls("jira").is_empty());
    }
}

#[test]
fn github_repository_permissions_are_checked_after_identity_validation() {
    let fixture = Fixture::new();
    fixture.mode("gh", "repo-denied");
    let report = report(fixture.doctor().args(["--check", "github"]), 1);
    for kind in [ReferenceKind::GitHubIssue, ReferenceKind::GitHubPullRequest] {
        let github = provider(&report, kind);
        assert_eq!(github["state"], "access_denied");
        assert!(lines(&github["details"]).contains("authenticated identity validated"));
    }
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 4);
    assert_eq!(
        calls
            .iter()
            .map(|call| call[1].as_str())
            .collect::<Vec<_>>(),
        ["api", "repo", "api", "repo"]
    );
    assert!(fixture.calls("jira").is_empty());
}

#[test]
fn github_remote_discovery_determines_the_enterprise_identity_host_without_account_sweeps() {
    let fixture = Fixture::new();
    let report = report(
        fixture
            .doctor()
            .env_remove("GH_HOST")
            .env_remove("GH_REPO")
            .args(["--check", "github"]),
        0,
    );
    assert_eq!(
        provider(&report, ReferenceKind::GitHubIssue)["state"],
        "ready"
    );
    assert_eq!(
        provider(&report, ReferenceKind::GitHubPullRequest)["state"],
        "ready"
    );
    let calls = fixture.calls("gh");
    assert_eq!(calls.len(), 8);
    for sequence in calls.chunks(4) {
        assert_eq!(
            sequence[0][1..],
            ["repo", "view", "--json", "nameWithOwner,url"]
        );
        assert_eq!(sequence[1][1..], ["api", "user", "--hostname", GH_HOST]);
        assert_eq!(sequence[2][1], "repo");
    }
    assert!(calls.iter().all(|call| {
        !call
            .iter()
            .any(|argument| argument == "auth" || argument == "token")
    }));
    assert!(fixture.calls("jira").is_empty());
}
