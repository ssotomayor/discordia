#[macro_export]
macro_rules! dlog {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            $crate::devlog::write_line(&format!($($arg)*));
        }
        #[cfg(not(debug_assertions))]
        {
            if false {
                let _ = format_args!($($arg)*);
            }
        }
    };
}

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
        let dir = path();
        if let Some(parent) = dir.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        eprintln!("[devlog] writing to {}", dir.display());
    });

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
    #[test]
    fn writes_a_line_even_when_the_config_dir_is_missing() {
        let tmp =
            std::env::temp_dir().join(format!("dioxusfun-devlog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("DIOXUSFUN_CONFIG_DIR", &tmp) };

        assert!(!tmp.exists(), "precondition: config dir absent");
        super::write_line("hello from the test");

        let body = std::fs::read_to_string(super::path()).expect("dev.log was created");
        assert!(
            body.contains("hello from the test"),
            "line written: {body:?}"
        );
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
