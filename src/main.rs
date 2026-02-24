mod config;
mod fs;
mod inode_map;
mod intercept;
mod provider;

use std::os::fd::FromRawFd;
use std::os::fd::OwnedFd;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use clap::Parser;
use fuser::MountOption;

use crate::config::Config;
use crate::fs::ProxyFs;
use crate::intercept::InterceptMatcher;
use crate::provider::SqliteProvider;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .with_writer(std::io::stderr)
        .init();

    let config = Config::parse();

    let dir = config.dir.canonicalize()?;
    anyhow::ensure!(dir.is_dir(), "not a directory: {}", dir.display());

    let db_path = config
        .db
        .canonicalize()
        .unwrap_or_else(|_| config.db.clone());

    // Open root fd BEFORE mounting to avoid deadlock
    let root_fd = {
        let c_path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes())?;
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_PATH | libc::O_DIRECTORY) };
        anyhow::ensure!(
            fd >= 0,
            "failed to open root dir: {}",
            std::io::Error::last_os_error()
        );
        unsafe { OwnedFd::from_raw_fd(fd) }
    };

    let provider = SqliteProvider::new(db_path);
    provider.ensure_schema()?;

    let matcher = InterceptMatcher::new(&config.patterns)?;

    let proxy = ProxyFs::new(root_fd, matcher, Box::new(provider));

    let mut fuse_config = fuser::Config::default();
    fuse_config.mount_options = vec![
        MountOption::AutoUnmount,
        MountOption::FSName("dycon".to_owned()),
        MountOption::RW,
    ];
    fuse_config.acl = fuser::SessionACL::RootAndOwner;

    tracing::info!("mounting on {}", dir.display());

    let session = fuser::spawn_mount2(proxy, &dir, &fuse_config)?;

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    ctrlc::set_handler(move || {
        eprintln!("\nunmounting...");
        r.store(false, Ordering::SeqCst);
    })?;

    while running.load(Ordering::SeqCst) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }

    drop(session);
    tracing::info!("unmounted {}", dir.display());

    Ok(())
}
