use kiro_lb::upstream::endpoints::{
    cooldown_remaining, fastest_order, record_failure_backoff, record_latency, record_success,
    MAX_BACKOFF_SECONDS,
};
use serde_json::json;

fn order() -> Vec<String> {
    ["runtime", "codewhisperer", "amazonq"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect()
}

#[test]
fn a_challenger_leads_only_when_more_than_15_percent_faster() {
    let dir =
        std::env::temp_dir().join(format!("kirolb-fastest-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("DASHBOARD_DATA_DIR", &dir);
    kiro_lb::store::initialize().unwrap();

    assert_eq!(
        fastest_order("eu-west-9", &order()),
        order(),
        "unmeasured keeps the manual order"
    );

    record_latency(
        "eu-west-9",
        "m",
        &[
            ("runtime", 1000.0),
            ("codewhisperer", 1200.0),
            ("amazonq", 1500.0),
        ],
    );
    assert_eq!(fastest_order("eu-west-9", &order())[0], "runtime");

    record_latency(
        "eu-west-9",
        "m",
        &[("runtime", 1000.0), ("codewhisperer", 900.0)],
    );
    assert_eq!(
        fastest_order("eu-west-9", &order())[0],
        "runtime",
        "10% faster is not enough to take the lead"
    );

    record_latency(
        "eu-west-9",
        "m",
        &[("runtime", 1000.0), ("codewhisperer", 800.0)],
    );
    assert_eq!(fastest_order("eu-west-9", &order())[0], "codewhisperer");
    assert_eq!(
        fastest_order("ap-south-9", &order())[0],
        "runtime",
        "regions are independent"
    );

    let saved = kiro_lb::store::load_setting("endpoint_latency").unwrap();
    assert_eq!(saved["eu-west-9"]["leader"], json!("codewhisperer"));
}

#[test]
fn failure_cooldown_doubles_up_to_ten_minutes_and_resets_on_success() {
    let mut last = 0.0;
    for expected in [30.0, 60.0, 120.0, 240.0, 480.0, 600.0, 600.0] {
        last = record_failure_backoff("amazonq", 30.0);
        assert_eq!(last, expected);
    }
    assert_eq!(last, MAX_BACKOFF_SECONDS);
    assert!(cooldown_remaining("amazonq") > 590.0);
    record_success("acct", "m", "amazonq");
    assert_eq!(cooldown_remaining("amazonq"), 0.0);
    assert_eq!(record_failure_backoff("amazonq", 30.0), 30.0);
}
