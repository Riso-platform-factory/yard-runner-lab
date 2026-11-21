//! One test: a start that ends with `yard_log::exit` writes the whole run
//! to stderr, its open group ended as interrupted and one `run-end` last,
//! on every system. It runs itself again as a child process, which
//! installs the process-wide logger and exits.

use std::process::Command;

/// Set in the child process, which logs and exits.
const CHILD: &str = "YARD_LOG_EXIT_TEST_CHILD";

#[test]
fn exit_ends_the_run_before_the_process_exits() {
    if std::env::var_os(CHILD).is_some() {
        yard_log::start(yard_product::Product {
            name: "sample",
            version: "1",
            identifier: "com.example.sample",
        });
        let _group = tracing::info_span!("hand-over").entered();
        tracing::info!("handed over");
        yard_log::exit(3);
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "exit_ends_the_run_before_the_process_exits",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env("SAMPLE_LIFECYCLE", "info")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("handed over"), "{stderr}");
    let last = stderr
        .lines()
        .rfind(|line| !line.trim().is_empty())
        .unwrap();
    assert!(last.contains("run-end"), "{stderr}");
    let run_ends = stderr
        .lines()
        .filter(|line| line.contains("run-end"))
        .count();
    assert_eq!(run_ends, 1, "{stderr}");
    // The group still open when the process exits ends as interrupted.
    assert!(
        stderr
            .lines()
            .any(|line| line.contains("⊘ [") && line.contains("hand-over]")),
        "{stderr}"
    );
}
