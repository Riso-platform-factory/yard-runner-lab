//! Without the lifecycle trace (a build without debug assertions and no
//! `SAMPLE_LIFECYCLE`), no `tracing` event reaches the diagnostic log, even
//! with `tracing`'s `log` feature on, which sends events to `log` while no
//! dispatcher has been set.
//!
//! Debug builds always run the lifecycle trace, so this reaches the case only
//! when `yard_log` is built without debug assertions:
//! `cargo test -p yard_log --test no_lifecycle_trace --config
//! 'profile.dev.package.yard_log.debug-assertions=false'`.
//!
//! This file installs the process-wide logger and subscriber, so it holds one
//! test.

use std::time::Duration;

#[test]
fn no_tracing_event_reaches_the_diagnostic_log() {
    let home = tempfile::tempdir().unwrap();
    // SAFETY: this file holds one test, and it has started no thread yet.
    unsafe {
        std::env::remove_var("SAMPLE_LIFECYCLE");
        std::env::remove_var("SAMPLE_LOG");
    }
    yard_log::start(yard_product::Product {
        name: "sample",
        version: "9.9.9",
        identifier: "com.example.sample",
    });
    {
        let _group = tracing::info_span!("sample-group").entered();
        tracing::info!("traced info");
        tracing::warn!("traced warn");
        tracing::error!("traced error");
    }
    log::warn!("logged warn");
    let logs = home.path().join("logs");
    std::fs::create_dir_all(&logs).unwrap();
    yard_log::open_files(&logs);
    log::error!("logged error");

    let path = logs.join("sample.log");
    let mut content = String::new();
    for _ in 0..200 {
        content = std::fs::read_to_string(&path).unwrap_or_default();
        if content.contains("logged error") {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(content.contains("logged warn"), "{content}");
    assert!(content.contains("logged error"), "{content}");
    assert!(!content.contains("traced"), "{content}");
    assert!(!content.contains("sample-group"), "{content}");
}
