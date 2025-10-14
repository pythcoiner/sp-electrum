use std::{
    collections::{BTreeMap, BTreeSet},
    ops::RangeInclusive,
    sync::{mpsc, Arc, Mutex},
};

use crate::{
    block::{Notification, SpendableTx},
    threading::{Context, WorkerPool},
};

pub struct State {
    txs: BTreeMap<u64 /* height */, Vec<SpendableTx>>,
    scanning: BTreeSet<u64>,
    scan_tip: u64,
    start_height: u64,
    target_height: u64,
    updater: Option<mpsc::Sender<SpendableTx>>,
}

impl State {
    pub fn new(range: &RangeInclusive<u64>) -> Self {
        let txs = BTreeMap::new();
        let scanning = BTreeSet::new();
        let start_height = *range.start();
        let scan_tip = start_height - 1;
        let target_height = *range.end();
        Self {
            txs,
            scanning,
            scan_tip,
            start_height,
            target_height,
            updater: None,
        }
    }
    pub fn progress(&self) -> f64 {
        let range = self.target_height - self.start_height;
        let done = self.scan_tip - self.start_height;
        done as f64 / range as f64
    }
}

pub struct Scanner {
    _pool: WorkerPool,
    state: Arc<Mutex<State>>,
}

impl Scanner {
    pub fn start(
        range: RangeInclusive<u64>,
        context: Context,
        workers: usize,
        threads: usize,
    ) -> Self {
        let state = Arc::new(Mutex::new(State::new(&range)));
        let mut _pool = WorkerPool::new(workers, threads, &context);
        let notif = _pool.start(range);
        Self::poll(notif, state.clone());
        Self { _pool, state }
    }

    fn poll(notif: mpsc::Receiver<Notification>, state: Arc<Mutex<State>>) {
        std::thread::Builder::new()
            // TODO: check stack size
            .stack_size(1024)
            .spawn(move || {
                // TODO:
            })
            .expect("failed to start poller");
    }

    pub fn progress(&self) -> f64 {
        self.state.lock().expect("poisoned").progress()
    }

    pub fn tip(&self) -> u64 {
        self.state.lock().expect("poisoned").scan_tip
    }

    pub fn txs(&self) -> BTreeMap<u64 /* height */, Vec<SpendableTx>> {
        self.state.lock().expect("poisoned").txs.clone()
    }

    pub fn register_updates(&mut self) -> Option<mpsc::Receiver<SpendableTx>> {
        if self.state.lock().expect("poisonend").updater.is_some() {
            return None;
        }

        let (updater, receiver) = mpsc::channel();
        self.state.lock().expect("poisoned").updater = Some(updater);
        Some(receiver)
    }
}
