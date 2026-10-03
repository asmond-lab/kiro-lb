use serde_json::json;

#[test]
fn release_defaults_turn_on_once_and_then_respect_the_operator() {
    std::env::remove_var("SHORTEN_CLAUDE_TOOLS");
    std::env::remove_var("CLAUDE_WRITE_HINT");
    std::env::remove_var("KIRO_ENDPOINT_ROTATION");
    let dir =
        std::env::temp_dir().join(format!("kirolb-defaults-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();
    kiro_lb::store::save_setting("shorten_claude_tools", &json!(false)).unwrap();
    kiro_lb::store::save_setting("endpoints", &json!({"rotation": false, "order": ["runtime"], "cooldownSeconds": 30, "strategy": "ordered"})).unwrap();

    kiro_lb::settings::load_all();
    let flags = kiro_lb::settings::prompt_flags();
    assert!(flags.shorten_tools && flags.write_hint);
    let endpoints = kiro_lb::settings::endpoint_settings();
    assert!(endpoints.rotation);
    assert_eq!(endpoints.strategy, "fastest");
    assert_eq!(endpoints.order, vec!["runtime"]);

    kiro_lb::settings::set_prompt_flag("claude_write_hint", false).unwrap();
    kiro_lb::settings::load_all();
    assert!(
        !kiro_lb::settings::prompt_flags().write_hint,
        "a later choice survives the next start"
    );
}
