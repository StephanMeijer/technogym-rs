use technogym::{Facility, FileSessionStore, MemorySessionStore, Session, SessionStore, UserInfo};

fn session() -> Session {
    Session {
        token: "tok".into(),
        user: UserInfo {
            user_id: "u1".into(),
            facilities: vec![Facility {
                id: "f1".into(),
                url: "my-gym".into(),
                name: "My Gym".into(),
            }],
            ..UserInfo::default()
        },
        saved_at: 42,
    }
}

#[tokio::test]
async fn file_store_round_trips_with_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path().join("nested"));
    assert!(store.load().await.unwrap().is_none());
    store.save(&session()).await.unwrap();
    assert_eq!(store.load().await.unwrap(), Some(session()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(store.path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(
        !std::fs::read_dir(store.dir()).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
    store.clear().await.unwrap();
}

#[tokio::test]
async fn file_store_rejects_half_valid_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSessionStore::new(dir.path());
    for text in [
        "not json",
        r#"{"token":"","user":{"userId":"u1","facilities":[]}}"#,
        r#"{"token":"t","user":{"facilities":[]}}"#,
    ] {
        std::fs::write(store.path(), text).unwrap();
        assert!(store.load().await.unwrap().is_none(), "{text}");
    }
    std::fs::write(
        store.path(),
        r#"{"token":"t","user":{"userId":"u1","facilities":[{"url":"g"}]}}"#,
    )
    .unwrap();
    let loaded = store.load().await.unwrap().unwrap();
    assert_eq!(loaded.user.facilities[0].url, "g");
    assert_eq!(loaded.user.first_name, "");
    assert_eq!(loaded.saved_at, 0);
}

#[tokio::test]
async fn memory_store() {
    let store = MemorySessionStore::default();
    assert!(store.load().await.unwrap().is_none());
    store.save(&session()).await.unwrap();
    assert!(store.load().await.unwrap().is_some());
    store.clear().await.unwrap();
    assert!(store.load().await.unwrap().is_none());
}
