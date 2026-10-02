use super::{
    callback::{cache_from_url, sso_header},
    model::validate_token,
    oauth::refresh_request,
    *,
};
use reqwest::Url;
use std::time::Instant;

#[test]
fn contended_authentication_lock_has_a_deadline_and_preserves_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let settings = Settings::default();
    let cache = settings.cache_path(&path);
    let saved = br#"{"access_token":"test-access","refresh_token":null,"expires_at":null,"client_authorization":null}"#;
    secure_write(&cache, saved).unwrap();
    let held = lock(
        &cache,
        &OperationControl::uninterrupted(Duration::from_secs(1)),
    )
    .map_err(OperationError::into_query)
    .unwrap();
    let started = Instant::now();
    let result = token(&settings, &path, Duration::from_millis(25), None);
    assert!(matches!(result, Err(QueryError::Response(_))));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(fs::read(&cache).unwrap(), saved);
    drop(held);
    assert_eq!(
        token(&settings, &path, Duration::from_secs(1), None)
            .unwrap()
            .access_token,
        "test-access"
    );
}

#[test]
fn cancellation_interrupts_a_contended_lock_without_consuming_credentials() {
    use std::cell::Cell;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let settings = Settings::default();
    let cache = settings.cache_path(&path);
    let held = lock(
        &cache,
        &OperationControl::uninterrupted(Duration::from_secs(1)),
    )
    .map_err(OperationError::into_query)
    .unwrap();
    let polls = Cell::new(0);
    let cancelled = || {
        polls.set(polls.get() + 1);
        Ok(polls.get() >= 4)
    };
    let control = OperationControl::new(Duration::from_secs(30), &cancelled);
    let started = Instant::now();
    assert!(matches!(
        token_controlled(&settings, &path, &control, None),
        Err(OperationError::Cancelled)
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(!cache.exists());
    drop(held);
}

#[test]
fn changed_accounts_and_unbound_legacy_caches_are_never_reused_or_refreshed() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    secure_write(&path, b"cas:\n  username: new-account\n  password: ''\n").unwrap();
    let settings = Settings::read(&path).unwrap();
    let cache_path = settings.cache_path(&path);
    let mut cache = TokenCache::from_response(
            &serde_json::json!({"access_token":"old-access","refresh_token":"old-refresh","expires_in":3600}),
            OAuthProvider::Berserker,
            Utc::now().timestamp(),
        ).unwrap();
    for account in [Some("old-account"), None] {
        cache.cas_username = account.map(str::to_owned);
        save_cache(&cache_path, &cache).unwrap();
        let before = fs::read(&cache_path).unwrap();
        // Empty CAS password fails locally. If the old token were reused this
        // would succeed; refreshing it would attempt a network request.
        assert!(matches!(
            token(&settings, &path, Duration::from_secs(1), None),
            Err(QueryError::AuthFlow(_))
        ));
        assert!(!status(&path).unwrap().cached);
        assert_eq!(fs::read(&cache_path).unwrap(), before);
    }
    cache.cas_username = Some("new-account".into());
    save_cache(&cache_path, &cache).unwrap();
    assert_eq!(
        token(&settings, &path, Duration::from_secs(1), None)
            .unwrap()
            .access_token,
        "old-access"
    );
    assert!(status(&path).unwrap().cached);
}

#[test]
fn imports_bind_to_the_configured_account_and_support_token_only_configs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let input = dir.path().join("response.json");
    secure_write(
        &input,
        br#"{"access_token":"imported-access","cas_username":"untrusted-input"}"#,
    )
    .unwrap();
    for (config, account) in [
        (
            "cas:\n  username: current-account\n  password: ''\n",
            Some("current-account"),
        ),
        ("cas: null\n", None),
        ("cas:\n  username: ''\n  password: ''\n", None),
    ] {
        secure_write(&path, config.as_bytes()).unwrap();
        import(&path, &input, OAuthProvider::Berserker, None).unwrap();
        let settings = Settings::read(&path).unwrap();
        let cache = load_cache(&settings.cache_path(&path)).unwrap().unwrap();
        assert_eq!(cache.cas_username.as_deref(), account);
        assert_eq!(
            token(&settings, &path, Duration::from_secs(1), None)
                .unwrap()
                .access_token,
            "imported-access"
        );
    }
}

#[test]
fn rejected_refresh_grants_relogin_and_replace_the_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let settings = Settings::default();
    let old = TokenCache::from_response(
        &serde_json::json!({"access_token":"old-access","refresh_token":"old-refresh"}),
        OAuthProvider::Berserker,
        0,
    )
    .unwrap();
    save_cache(&settings.cache_path(&path), &old).unwrap();
    let mut refreshed = false;
    let mut logged_in = false;
    let new = token_with(
        &settings,
        &path,
        Some("old-access"),
        |_| {
            refreshed = true;
            Err(QueryError::Authentication)
        },
        || {
            logged_in = true;
            TokenCache::from_response(
                &serde_json::json!({"access_token":"new-access","refresh_token":"new-refresh"}),
                OAuthProvider::Berserker,
                0,
            )
        },
    )
    .unwrap();
    assert!(refreshed && logged_in);
    assert_eq!(new.access_token, "new-access");
    assert_eq!(
        load_cache(&settings.cache_path(&path))
            .unwrap()
            .unwrap()
            .refresh_token
            .as_deref(),
        Some("new-refresh")
    );
}

#[test]
fn temporary_refresh_failures_remain_retryable() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    let cases = [
        ("408 Request Timeout", r#"{}"#),
        ("429 Too Many Requests", r#"{}"#),
        (
            "400 Bad Request",
            r#"{"error":"temporarily_unavailable","error_description":"must-not-be-logged"}"#,
        ),
        (
            "200 OK",
            r#"{"error":"temporarily_unavailable","error_description":"must-not-be-logged"}"#,
        ),
        ("200 OK", r#"{"code":500}"#),
        ("200 OK", "<html>service unavailable</html>"),
        ("200 OK", r#"{}"#),
    ];
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for (status, body) in cases {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap();
                    if body.len() >= length {
                        break;
                    }
                }
            }
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    let old = TokenCache::from_response(
        &serde_json::json!({"access_token":"access","refresh_token":"refresh"}),
        OAuthProvider::Berserker,
        0,
    )
    .unwrap();
    let client = crate::client(Duration::from_secs(5)).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    let settings = Settings::default();
    let cache_path = settings.cache_path(&path);
    save_cache(&cache_path, &old).unwrap();
    let before = fs::read(&cache_path).unwrap();
    for (status, _) in cases {
        let error = token_with(
            &settings,
            &path,
            Some("access"),
            |cache| refresh_request(&client, cache, &endpoint, "Basic dGVzdDpwdWJsaWM="),
            || panic!("temporary refresh failure must not trigger CAS"),
        )
        .err()
        .unwrap();
        if status.starts_with("200") {
            assert!(matches!(error, QueryError::Response(_)), "{error:?}");
        } else {
            assert!(matches!(error, QueryError::Http(_)), "{error:?}");
        }
        assert!(!error.to_string().contains("must-not-be-logged"));
        assert_eq!(fs::read(&cache_path).unwrap(), before);
    }
    server.join().unwrap();
}

#[test]
fn device_initialization_is_stable_and_preserves_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.yaml");
    secure_write(&path,b"cas:\n  username: test-account\n  password: 'test:password#value'\n  device_id: null\nextra: retained\n").unwrap();
    let first = credentials(&path).unwrap();
    let second = credentials(&path).unwrap();
    assert_eq!(first.device_id, second.device_id);
    assert_eq!(second.username, "test-account");
    assert_eq!(second.password, "test:password#value");
    let saved: serde_yaml::Value = serde_yaml::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["extra"].as_str(), Some("retained"));
}

#[test]
fn sso_component_uses_the_authorization_after_the_sso_marker() {
    let script = r#"Authorization:"Basic ZGVjb3k6cHVibGlj", logintype:"sso", device_token:"h5"; url:"/berserker-auth/oauth/token", headers:{Authorization:"Basic dGVzdDpwdWJsaWM="}"#;
    assert_eq!(sso_header(script).unwrap(), "Basic dGVzdDpwdWJsaWM=");
    assert!(sso_header("unknown component").is_err());
}

#[test]
fn refresh_http_grant_rotates_and_errors_preserve_the_old_cache() {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/oauth/token", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        for (status, body) in [
            (
                "200 OK",
                r#"{"access_token":"new-access","refresh_token":"rotated-refresh","expires_in":600}"#,
            ),
            (
                "400 Bad Request",
                r#"{"error":"invalid_grant","error_description":"must-not-be-logged"}"#,
            ),
            ("503 Service Unavailable", r#"{}"#),
            ("401 Unauthorized", r#"{}"#),
            ("403 Forbidden", r#"{}"#),
            ("200 OK", r#"{"error":"invalid_grant"}"#),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                let text = String::from_utf8_lossy(&bytes);
                if let Some((headers, body)) = text.split_once("\r\n\r\n") {
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|n| n.parse::<usize>().ok())
                        })
                        .unwrap();
                    if body.len() >= length {
                        let authorization = headers
                            .lines()
                            .filter_map(|line| line.split_once(':'))
                            .find(|(key, _)| key.eq_ignore_ascii_case("authorization"))
                            .unwrap()
                            .1
                            .trim();
                        assert_eq!(authorization, "Basic dGVzdDpwdWJsaWM=");
                        assert!(body.contains("grant_type=refresh_token"));
                        assert!(body.contains("refresh_token=old%2Brefresh%26value"));
                        break;
                    }
                }
            }
            write!(stream,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    let mut old = TokenCache::from_response(
        &serde_json::json!({"access_token":"old-access","refresh_token":"old+refresh&value"}),
        OAuthProvider::Berserker,
        0,
    )
    .unwrap();
    old.cas_username = Some("current-account".into());
    let client = crate::client(Duration::from_secs(5)).unwrap();
    let new = refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM=").unwrap();
    assert_eq!(new.access_token, "new-access");
    assert_eq!(new.refresh_token.as_deref(), Some("rotated-refresh"));
    assert_eq!(new.cas_username.as_deref(), Some("current-account"));
    let error = refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM=")
        .err()
        .unwrap();
    assert!(matches!(error, QueryError::Authentication));
    assert!(!error.to_string().contains("must-not-be-logged"));
    assert!(matches!(
        refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM="),
        Err(QueryError::Http(503))
    ));
    for _ in 0..3 {
        assert!(matches!(
            refresh_request(&client, &old, &endpoint, "Basic dGVzdDpwdWJsaWM="),
            Err(QueryError::Authentication)
        ));
    }
    assert_eq!(old.refresh_token.as_deref(), Some("old+refresh&value"));
    server.join().unwrap();
}

#[test]
fn a_concurrent_new_token_is_used_without_refreshing_again() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.yaml");
    let settings = Settings::default();
    let mut current = TokenCache::from_response(
        &serde_json::json!({"access_token":"new-access","expires_in":3600}),
        OAuthProvider::Berserker,
        Utc::now().timestamp(),
    )
    .unwrap();
    current.refresh_token = Some("current-refresh".into());
    save_cache(&settings.cache_path(&config_path), &current).unwrap();
    let found = token(
        &settings,
        &config_path,
        Duration::from_secs(1),
        Some("old-access"),
    )
    .unwrap();
    assert_eq!(found.access_token, "new-access");
}

#[test]
fn expired_token_without_credentials_requires_local_login_and_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("config.yaml");
    secure_write(&config_path, b"cas: null\n").unwrap();
    let settings = Settings::default();
    let old = TokenCache::from_response(
        &serde_json::json!({"access_token":"old-access","expires_in":1}),
        OAuthProvider::Berserker,
        0,
    )
    .unwrap();
    let path = settings.cache_path(&config_path);
    save_cache(&path, &old).unwrap();
    let before = fs::read(&path).unwrap();
    assert!(matches!(
        token(&settings, &config_path, Duration::from_secs(1), None),
        Err(QueryError::AuthFlow(_))
    ));
    assert_eq!(fs::read(&path).unwrap(), before);
}
#[test]
fn expiry_and_rotating_refresh_tokens() {
    let old=TokenCache::from_response(&serde_json::json!({"access_token":"access1","refresh_token":"refresh1","expires_in":"600"}),OAuthProvider::Berserker,100).unwrap();
    assert!(old.usable(100));
    assert!(!old.usable(400));
    let new=TokenCache::from_response(&serde_json::json!({"data":{"access_token":"access2","refresh_token":"refresh2","expires_in":900}}),old.provider,400).unwrap().merged_refresh(&old);
    assert_eq!(new.refresh_token.as_deref(), Some("refresh2"));
    let omitted = TokenCache::from_response(
        &serde_json::json!({"access_token":"access3"}),
        old.provider,
        100,
    )
    .unwrap()
    .merged_refresh(&old);
    assert_eq!(omitted.refresh_token.as_deref(), Some("refresh1"));
}
#[test]
fn token_url_and_fragment_are_parsed_without_logging() {
    let url=Url::parse("https://mcard.sdu.edu.cn/charge-app/#access_token=example&refresh_token=refresh&expires_in=900").unwrap();
    let cache = cache_from_url(&url).unwrap();
    assert_eq!(cache.access_token, "example");
    assert!(cache.refresh_token.is_some());
}
#[test]
fn oauth_error_does_not_become_a_token() {
    assert!(
        TokenCache::from_response(
            &serde_json::json!({"error":"invalid_grant","error_description":"secret-value"}),
            OAuthProvider::Berserker,
            0
        )
        .is_err()
    );
    assert!(validate_token("bad\r\nheader").is_err());
}
#[test]
fn atomic_cache_has_private_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("nested/auth.json");
    secure_write(&path, b"one").unwrap();
    secure_write(&path, b"two").unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"two");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
