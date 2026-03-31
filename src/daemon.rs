use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

use tokio::time::sleep;

use crate::service::Supervisor;

pub async fn run_daemon(supervisor: Arc<Supervisor>, stop_flag: Arc<AtomicBool>) {
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let _ = supervisor.poll_once().await;

        let sleep_duration = Duration::from_secs(supervisor.poll_interval_secs());
        let mut elapsed = Duration::from_secs(0);

        while elapsed < sleep_duration {
            if stop_flag.load(Ordering::Relaxed) {
                return;
            }

            let step = Duration::from_millis(500);
            sleep(step).await;
            elapsed += step;
        }
    }
}
