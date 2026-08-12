//! Append-only debug log, for tracking down things that only misbehave in a
//! real session.
//!
//! `eprintln!` is fine when you're watching `dx serve`, but useless for a bug
//! you notice three minutes into a call and want to read about afterwards — the
//! interesting lines have already scrolled past, and reproducing costs another
//! call. So debug builds also tee to a file next to `settings.json`.
//!
//! Deliberately **debug-only**: `dlog!` compiles to nothing in a release build,
//! so a shipped client never grows an unbounded log of who was in which voice
//! channel. Nothing here is on the audio path — calls sit in the command loop
//! and in Dioxus effects, not in the cpal callback or the 10ms hop loop.

/// Write one timestamped line to the dev log, `println!`-style.
///
/// No-op in release. Failures are swallowed: a diagnostic that can break the
/// app it is diagnosing is worse than no diagnostic.
///
/// The release arm still *reads* its arguments, and has to. Expanding to
/// nothing at all makes a binding that exists only to be logged look unused to
/// the compiler, so every call site pays for the macro with an
/// `unused_variables` warning that only appears in release builds — where no
/// lint gate runs, since clippy is a `cargo` dev-profile job. `format_args!`
/// borrows the arguments and builds nothing; `let _ =` drops it at the end of
/// the statement. Nothing is formatted, allocated or emitted.
#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            $crate::devlog::write_line(&format!($($arg)*));
        }
        #[cfg(not(debug_assertions))]
        {
            let _ = format_args!($($arg)*);
        }
    };
}

/// Where the log lands. Printed once on first write so it can actually be found.
#[cfg(debug_assertions)]
pub fn path() -> std::path::PathBuf {
    crate::identity::config_dir().join("dev.log")
}

#[cfg(debug_assertions)]
pub fn write_line(msg: &str) {
    use std::io::Write;
    use std::sync::Once;

    static ANNOUNCE: Once = Once::new();
    ANNOUNCE.call_once(|| {
        // `config_dir()` only computes a path — on a fresh profile nothing has
        // created it yet, and an append to a file in a missing directory just
        // fails. Silently, which would cost a whole repro session before anyone
        // noticed the log was empty.
        let dir = path();
        if let Some(parent) = dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        eprintln!("[devlog] writing to {}", dir.display());
    });

    // Seconds since the epoch rather than a formatted clock: this file is read
    // by diffing timestamps between lines, and pulling in a date formatter for
    // that is not worth a dependency.
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path())
    else {
        return;
    };
    let _ = writeln!(f, "{t} {msg}");
}

#[cfg(all(test, debug_assertions))]
mod tests {
    /// The failure this guards against is the silent one: a log that compiles,
    /// runs, and writes nothing because the directory wasn't there.
    #[test]
    fn writes_a_line_even_when_the_config_dir_is_missing() {
        let tmp =
            std::env::temp_dir().join(format!("dioxusfun-devlog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        // SAFETY: single-threaded test, and the var is read through
        // `config_dir()` only while this test holds it.
        unsafe { std::env::set_var("DIOXUSFUN_CONFIG_DIR", &tmp) };

        assert!(!tmp.exists(), "precondition: config dir absent");
        super::write_line("hello from the test");

        let body = std::fs::read_to_string(super::path()).expect("dev.log was created");
        assert!(
            body.contains("hello from the test"),
            "line written: {body:?}"
        );
        // Timestamp prefix, so lines can be diffed for ordering.
        assert!(
            body.split_whitespace()
                .next()
                .is_some_and(|t| t.parse::<u128>().is_ok()),
            "line starts with a millisecond timestamp: {body:?}"
        );

        unsafe { std::env::remove_var("DIOXUSFUN_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
