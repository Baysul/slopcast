use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const MAX_RUNTIME_LOG_BYTES: u64 = 5_000_000;

/// Saves native GTK/CEF stderr separately from application console messages.
///
/// # Errors
/// Returns an error if the runtime log cannot be opened or stderr cannot be redirected.
pub fn configure_native_output() -> io::Result<()> {
    // The AppImage builder discovers dlopened libraries through LD_DEBUG output.
    // Preserve that stream for packaging and explicit native-loader debugging.
    if std::env::var_os("LD_DEBUG").is_some() {
        return Ok(());
    }
    let directory = dirs::data_local_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "user data directory unavailable"))?
        .join("slopcast/logs");
    fs::create_dir_all(&directory)?;
    let subprocess = std::env::args_os().any(|arg| arg.to_string_lossy().starts_with("--type="));
    redirect_stderr(&directory.join("runtime.log"), !subprocess)
}

fn redirect_stderr(path: &Path, rotate: bool) -> io::Result<()> {
    if rotate && fs::metadata(path).is_ok_and(|metadata| metadata.len() >= MAX_RUNTIME_LOG_BYTES) {
        fs::rename(path, path.with_extension("log.1"))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    io::stderr().flush()?;
    // SAFETY: file owns a valid descriptor, and dup2 duplicates it onto stderr.
    // The duplicated descriptor stays open after file is dropped and is inherited
    // by Chromium subprocesses, including messages emitted before Rust initializes.
    if unsafe { libc::dup2(file.as_raw_fd(), libc::STDERR_FILENO) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_diagnostics_preserve_stdout_and_append_to_file() -> io::Result<()> {
        const CHILD_LOG: &str = "SLOPCAST_TEST_RUNTIME_LOG";
        if let Some(path) = std::env::var_os(CHILD_LOG) {
            redirect_stderr(Path::new(&path), false)?;
            eprintln!("native runtime diagnostic");
            println!("visible application error");
            return Ok(());
        }

        let directory = tempfile::tempdir()?;
        let path = directory.path().join("runtime.log");
        fs::write(&path, "previous diagnostic\n")?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "diagnostics::tests::native_diagnostics_preserve_stdout_and_append_to_file",
                "--nocapture",
            ])
            .env(CHILD_LOG, &path)
            .output()?;
        assert!(output.status.success());
        assert_eq!(output.stderr.as_slice(), b"");
        assert!(String::from_utf8_lossy(&output.stdout).contains("visible application error"));
        assert_eq!(
            fs::read_to_string(path)?,
            "previous diagnostic\nnative runtime diagnostic\n"
        );
        Ok(())
    }

    #[test]
    fn loader_tracing_keeps_stderr_available_to_the_bundler() -> io::Result<()> {
        const CHILD: &str = "SLOPCAST_TEST_LOADER_TRACE";
        if std::env::var_os(CHILD).is_some() {
            configure_native_output()?;
            eprintln!("native loader trace remains visible");
            return Ok(());
        }
        let directory = tempfile::tempdir()?;
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "diagnostics::tests::loader_tracing_keeps_stderr_available_to_the_bundler",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("LD_DEBUG", "libs")
            .env("XDG_DATA_HOME", directory.path())
            .output()?;
        assert!(output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("native loader trace remains visible")
        );
        assert!(!directory.path().join("slopcast/logs/runtime.log").exists());
        Ok(())
    }
}
