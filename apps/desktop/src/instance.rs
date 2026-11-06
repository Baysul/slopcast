//! Per-settings-directory instance ownership and second-launch activation.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::services::Signal;
use fs2::FileExt;

pub struct Instance {
    _lock: File,
    port: u16,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Instance {
    /// `None` means an existing instance was activated successfully.
    pub fn acquire(
        directory: &Path,
        signals: async_channel::Sender<Signal>,
    ) -> Result<Option<Self>, String> {
        std::fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut lock = options
            .open(directory.join("desktop.lock"))
            .map_err(|e| e.to_string())?;
        if let Err(error) = lock.try_lock_exclusive() {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(format!("Cannot lock the desktop session: {error}"));
            }
            for _ in 0..20 {
                lock.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
                let mut address = String::new();
                lock.read_to_string(&mut address)
                    .map_err(|e| e.to_string())?;
                if let Ok(port) = address.trim().parse::<u16>()
                    && let Ok(mut stream) =
                        TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                {
                    stream.write_all(b"activate").map_err(|e| e.to_string())?;
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return Err(
                "Another Slopcast instance owns these settings but did not answer activation."
                    .into(),
            );
        }
        let listener =
            TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        lock.set_len(0).map_err(|e| e.to_string())?;
        write!(lock, "{port}").map_err(|e| e.to_string())?;
        lock.flush().map_err(|e| e.to_string())?;
        let running = Arc::new(AtomicBool::new(true));
        let active = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name("desktop-activation".into())
            .spawn(move || {
                for connection in listener.incoming() {
                    if !active.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(mut connection) = connection {
                        let _ = connection.set_read_timeout(Some(Duration::from_millis(500)));
                        let mut message = [0; 8];
                        if connection.read_exact(&mut message).is_ok() && &message == b"activate" {
                            let _ = signals.try_send(Signal::ShowMain);
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Some(Self {
            _lock: lock,
            port,
            running,
            thread: Some(thread),
        }))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        let _ = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, self.port));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_launch_activates_the_owner_and_restart_releases_the_lock() {
        let directory = tempfile::tempdir().unwrap();
        let (send, receive) = async_channel::bounded(4);
        let first = Instance::acquire(directory.path(), send.clone())
            .unwrap()
            .unwrap();
        assert!(
            Instance::acquire(directory.path(), send.clone())
                .unwrap()
                .is_none()
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            if let Ok(signal) = receive.try_recv() {
                assert!(matches!(signal, Signal::ShowMain));
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Owner did not receive activation"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(first);
        assert!(Instance::acquire(directory.path(), send).unwrap().is_some());
    }

    #[test]
    fn isolated_settings_have_independent_presenters() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let (send, _receive) = async_channel::bounded(4);
        let first = Instance::acquire(a.path(), send.clone()).unwrap();
        let second = Instance::acquire(b.path(), send).unwrap();
        assert!(first.is_some() && second.is_some());
    }
}
