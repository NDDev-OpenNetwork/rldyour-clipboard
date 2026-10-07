//! One coarse timer owns retention and idle exit; capture/list operations do
//! not scan the archive or create a cleanup timer of their own.
use crate::outbox::Watchers;
use crate::proto::Response;
use crate::store::Store;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

pub fn expire(store: &Store, watchers: &Watchers, retention: Option<i64>) {
    let Some(seconds) = retention else { return };
    let cutoff = crate::session::now().saturating_sub(seconds);
    // At most 1024 rows per wake. Large old archives drain incrementally so
    // maintenance cannot monopolise a low-end desktop's disk or index lock.
    for _ in 0..4 {
        match store.expire_before(cutoff) {
            Ok(removed) => {
                let count = removed.len();
                for entry in removed {
                    watchers.broadcast(&Response::Removed { entry });
                }
                if count < 256 {
                    break;
                }
                std::thread::yield_now();
            }
            Err(error) => {
                eprintln!("rldyour-clipboardd: retention deferred: {error}");
                break;
            }
        }
    }
}

pub fn spawn(store: Arc<Store>, watchers: Arc<Watchers>, retention: Option<i64>, idle_exit: bool) {
    if retention.is_none() && !idle_exit {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("maintenance".into())
        .stack_size(64 * 1024)
        .spawn(move || {
            let mut idle_since = Instant::now();
            loop {
                std::thread::sleep(crate::config::MAINTENANCE_INTERVAL);
                expire(&store, &watchers, retention);
                if crate::server::CONNECTED.load(Ordering::SeqCst) > 0 {
                    idle_since = Instant::now();
                } else if idle_exit && idle_since.elapsed() >= Duration::from_secs(120) {
                    std::process::exit(0);
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("rldyour-clipboardd: cannot start maintenance: {error}");
    }
}
