use kiro_lb::auth::{AuthError, KiroAuth, Source};
use kiro_lb::pool::AccountManager;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;

fn social(id: &str) -> Value {
    json!({"type": "internal", "id": id, "credential": {
        "refreshToken": format!("refresh-{id}"),
        "accessToken": format!("access-{id}"),
        "expiresAt": "2999-01-01T00:00:00Z",
        "region": "us-east-1",
        "profileArn": format!("arn:aws:codewhisperer:us-east-1:000000000000:profile/{id}")
    }})
}

fn builder_id(id: &str) -> Value {
    let mut entry = social(id);
    entry["credential"]["clientId"] = json!("client");
    entry["credential"]["clientSecret"] = json!("secret");
    entry
}

#[tokio::test]
async fn a_social_login_flags_the_accounts_it_signed_out() {
    let dir = std::env::temp_dir().join(format!(
        "kirolb-social-probe-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
    let entries = [
        social("new"),
        social("revoked"),
        social("alive"),
        social("dead"),
        builder_id("builder"),
    ];
    kiro_lb::store::with(|c| kiro_lb::store::replace_account_sources(c, &entries, true)).unwrap();

    let pool = Arc::new(AccountManager::new(reqwest::Client::new()));
    pool.load_credentials();
    for id in ["new", "revoked", "alive", "dead", "builder"] {
        let auth = KiroAuth::new(
            Source::Internal(id.into()),
            "us-east-1",
            None,
            reqwest::Client::new(),
        )
        .unwrap();
        *pool.get(id).expect("account loaded").auth.lock() = Some(Arc::new(auth));
    }
    let far = kiro_lb::store::now_f64() + 3600.0;
    pool.get("dead").unwrap().state.lock().auth_dead_until = far;

    let probed = Arc::new(Mutex::new(Vec::new()));
    let signed_out = pool
        .probe_social_sessions_with("new", |auth| {
            let probed = probed.clone();
            async move {
                let profile = auth.profile_arn().unwrap();
                let id = profile.rsplit('/').next().unwrap().to_owned();
                probed.lock().push(id.clone());
                if id == "revoked" {
                    Err(AuthError::CredentialDead {
                        account: id,
                        status: 401,
                    })
                } else {
                    Ok(())
                }
            }
        })
        .await;
    let _ = std::fs::remove_dir_all(&dir);

    let mut probed = probed.lock().clone();
    probed.sort();
    assert_eq!(probed, ["alive", "revoked"]);
    assert_eq!(signed_out, ["revoked"]);
    let now = kiro_lb::store::now_f64();
    assert!(pool.get("revoked").unwrap().state.lock().auth_dead_until > now);
    assert_eq!(pool.get("alive").unwrap().state.lock().auth_dead_until, 0.0);
    assert_eq!(pool.get("new").unwrap().state.lock().auth_dead_until, 0.0);
    assert_eq!(pool.get("dead").unwrap().state.lock().auth_dead_until, far);
}
