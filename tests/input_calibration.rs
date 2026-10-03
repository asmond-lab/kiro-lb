use kiro_lb::input_calibration::{calibrate, calibrate_count, observe};

#[test]
fn start_of_turn_estimate_follows_what_kiro_reported() {
    assert_eq!(calibrate(Some(1), "claude-opus-5.5", 1000), 1000);

    observe(Some(1), "claude-opus-5-5[1m]", 299_600, 244_000);
    let next = calibrate(Some(1), "claude-opus-5.5", 300_000);
    assert!((next - 244_326).abs() <= 1, "{next}");

    let fresh = calibrate(Some(2), "claude-opus-5.5", 10_000);
    assert!(
        fresh < 10_000 && fresh > 8_000,
        "new conversations use the model average: {fresh}"
    );
    assert_eq!(calibrate(Some(3), "claude-sonnet-4.6", 5_000), 5_000);

    observe(Some(1), "claude-opus-5.5", 100, 10_000);
    assert_eq!(
        calibrate(Some(1), "claude-opus-5.5", 100),
        200,
        "ratio is clamped"
    );
}

#[test]
fn count_tokens_ratio_is_sampled_not_live() {
    observe(None, "claude-haiku-4.5", 1000, 1200);
    let first = calibrate_count("claude-haiku-4.5", 1000);
    assert_eq!(first, 1200);
    for _ in 0..20 {
        observe(None, "claude-haiku-4.5", 1000, 700);
    }
    assert_eq!(
        calibrate_count("claude-haiku-4.5", 1000),
        first,
        "count_tokens keeps its sample for 10 minutes"
    );
}
