use rtosu_dataprovider::OsuReader;
use rtosu_dataprovider::schema_parity::{check_all_schemas, fetch_tosu_endpoint};

#[test]
fn test_schema_level_parity_against_tosu() {
    // 1. Check if tosu is running on 24050
    let tosu_live = fetch_tosu_endpoint(24050, "/json/v2");
    let Ok(_sample) = tosu_live else {
        println!(
            "[SKIPPED] tosu is not running on 127.0.0.1:24050; skipping live schema parity test"
        );
        return;
    };

    // 2. Check if osu! is running and poll state from rtosu
    let mut reader = match OsuReader::builder().build() {
        Ok(r) => r,
        Err(err) => {
            println!("[SKIPPED] could not initialize OsuReader: {err}; skipping test");
            return;
        }
    };

    let packet = match reader.poll() {
        Ok(p) => p,
        Err(err) => {
            println!("[SKIPPED] poll failed: {err}; skipping test");
            return;
        }
    };

    if !reader.is_attached() {
        println!("[SKIPPED] osu! is not attached; skipping schema parity test");
        return;
    }

    // 3. Run schema parity across all 4 shapes:
    // - /json/v2 (except settings)
    // - /json/v2/precise
    // - /json/v1
    // - /json/sc
    let reports = check_all_schemas(24050, &packet).expect("schema check failed");

    let mut any_failure = false;
    for report in &reports {
        println!("=== Parity Report for {} ===", report.endpoint);
        if report.passed {
            println!("PASS: Schema perfectly matches tosu (0 drifts)");
        } else {
            any_failure = true;
            println!("FAIL: {} schema drift(s) detected:", report.drifts.len());
            for d in &report.drifts {
                println!("  - [{:?}] {}: {}", d.kind, d.path, d.detail);
            }
        }
    }

    assert!(
        !any_failure,
        "Schema drift detected between tosu and rtosu!"
    );
}
