//! Per-user rotating application logs, mirrored to the launching terminal.
use std::io::Write;
use std::path::PathBuf;

pub fn directory() -> Option<PathBuf> {
    dirs::data_local_dir().map(|root| root.join("slopcast/logs"))
}

struct LogOutput {
    file: Option<std::fs::File>,
}
impl Write for LogOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(file) = &mut self.file {
            let _ = file.write_all(bytes);
        }
        std::io::stderr().write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if let Some(file) = &mut self.file {
            let _ = file.flush();
        }
        std::io::stderr().flush()
    }
}

pub fn init() {
    let file = (|| -> std::io::Result<std::fs::File> {
        let root = directory().ok_or_else(|| std::io::Error::other("No log directory"))?;
        std::fs::create_dir_all(&root)?;
        let path = root.join("app.log");
        if std::fs::metadata(&path).is_ok_and(|metadata| metadata.len() > 5_000_000) {
            let previous = root.join("app.log.1");
            if previous.exists() {
                std::fs::remove_file(&previous)?;
            }
            std::fs::rename(&path, previous)?;
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    })()
    .ok();
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("info,zbus=warn,tracing::span=warn"),
    )
    .target(env_logger::Target::Pipe(Box::new(LogOutput { file })))
    .init();
}
