use std::process::Command;

#[test]
fn offline_inspection_returns_canonical_sources() {
    let output = Command::new(env!("CARGO_BIN_EXE_discordia-music"))
        .args([
            "https://youtu.be/dQw4w9WgXcQ?si=discarded",
            "https://tidal.com/track/123",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        rows[0]["canonical_url"],
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    );
    assert_eq!(rows[1]["provider"], "tidal");
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_batch_is_rejected_without_leaking_credentials() {
    let output = Command::new(env!("CARGO_BIN_EXE_discordia-music"))
        .args([
            "--probe",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://secret:password@soundcloud.com/user/track",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).unwrap();
    assert!(!error.contains("secret"));
    assert!(!error.contains("password"));
}

#[test]
fn unsupported_probe_exits_unsuccessfully_with_explicit_report() {
    let output = Command::new(env!("CARGO_BIN_EXE_discordia-music"))
        .args(["--probe", "https://tidal.com/track/123"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let rows: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(rows[0]["status"], "unsupported_probe");
    assert_eq!(rows[0]["audio_verified"], false);
    assert_eq!(rows[0]["playlist_entries_verified"], false);
}
