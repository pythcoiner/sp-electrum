use std::{
    ops::RangeInclusive,
    panic::UnwindSafe,
    sync::{
        mpsc::{self},
        Arc, Mutex,
    },
    thread,
};

use miniscript::bitcoin::Amount;
use silentpayments::{
    receiving::Receiver,
    secp256k1::{All, Secp256k1, SecretKey},
};

use crate::{
    block::{self, Error, Notification},
    client::BlockChainClient,
};

pub struct Context {
    pub sp_receiver: Receiver,
    pub b_scan: SecretKey,
    pub client: Box<dyn BlockChainClient>,
    pub dust: Amount,
    pub secp: Secp256k1<All>,
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Self {
            sp_receiver: self.sp_receiver.clone(),
            b_scan: self.b_scan,
            client: self.client.clone(),
            dust: self.dust,
            secp: self.secp.clone(),
        }
    }
}

impl Context {
    pub fn new(
        sp_receiver: Receiver,
        b_scan: SecretKey,
        client: Box<dyn BlockChainClient>,
        dust: Amount,
        secp: Secp256k1<All>,
    ) -> Self {
        Self {
            sp_receiver,
            b_scan,
            client,
            dust,
            secp,
        }
    }
}

pub struct ThreadPool {
    workers: Vec<thread::JoinHandle<()>>,
    sender: mpsc::Sender<Box<dyn FnOnce(Context) -> Context + Send + UnwindSafe + 'static>>,
    _parking: mpsc::Sender<Context>,
    _sp_receivers: Arc<Mutex<mpsc::Receiver<Context>>>,
}

type Task = Box<dyn FnOnce(Context) -> Context + Send + UnwindSafe + 'static>;

impl ThreadPool {
    pub fn new(size: usize, context: &Context) -> Self {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));

        let mut workers = Vec::with_capacity(size);

        let (parking, sp_receivers) = mpsc::channel();
        let sp_receivers = Arc::new(Mutex::new(sp_receivers));

        for _ in 0..size {
            let rx = Arc::clone(&rx);
            let parking = parking.clone();
            parking.send(context.clone()).expect("poisoned");
            let sp_receivers = sp_receivers.clone();

            workers.push(
                std::thread::Builder::new()
                    // TODO: verify stack size
                    .stack_size(1024)
                    .spawn(move || loop {
                        let sp_receiver = sp_receivers
                            .lock()
                            .expect("poisoned")
                            .recv()
                            .expect("closed");
                        let task: Task = match rx.lock().expect("poisoned").recv() {
                            Ok(task) => task,
                            Err(_) => {
                                // TODO: log
                                parking.send(sp_receiver).expect("closed");
                                break;
                            }
                        };
                        // // if std::panic::catch_unwind(task).is_err() {
                        // //     // TODO: log
                        // }
                        let recv = task(sp_receiver);
                        parking.send(recv).expect("closed");
                    })
                    .expect("must not fail"),
            );
        }
        ThreadPool {
            workers,
            sender: tx,
            _parking: parking,
            _sp_receivers: sp_receivers,
        }
    }

    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce(Context) -> Context + Send + UnwindSafe + 'static,
    {
        let _ = self.sender.send(Box::new(f));
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        let _ = &self.sender;
        for worker in &mut self.workers.drain(..) {
            worker.join().unwrap();
        }
    }
}

pub struct Worker {
    pool: ThreadPool,
    context: Context,
}

impl Worker {
    pub fn new(threads: usize, context: Context) -> Self {
        let pool = ThreadPool::new(threads, &context);
        Self {
            pool,
            context: context.clone(),
        }
    }
    pub fn scan(&self, height: u64, notif: mpsc::Sender<Notification>) {
        let block = match self.context.client.get_block(height) {
            Ok(b) => b,
            Err(_) => {
                notif
                    .send(Notification::Error((height, Error::Block)))
                    .expect("closed");
                return;
            }
        };

        block::block_scan(&self.pool, height, block, notif, &self.context);
    }
}

pub struct WorkerPool {
    workers: Arc<Mutex<mpsc::Receiver<Worker>>>,
    parking: mpsc::Sender<Worker>,
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        while let Ok(w) = self.workers.lock().expect("poisoned").try_recv() {
            drop(w);
        }
    }
}

impl WorkerPool {
    pub fn new(workers: usize, threads: usize, context: &Context) -> Self {
        let count = workers;
        let (parking, workers) = mpsc::channel();
        let workers = Arc::new(Mutex::new(workers));
        for _ in 0..count {
            let worker = Worker::new(threads, context.clone());
            parking.send(worker).expect("cannot fail");
        }
        Self { workers, parking }
    }
    fn scan(
        workers: Arc<Mutex<mpsc::Receiver<Worker>>>,
        parking: mpsc::Sender<Worker>,
        height: u64,
        notif: mpsc::Sender<Notification>,
    ) {
        // TODO: verify stack size
        let _ = std::thread::Builder::new().stack_size(1024).spawn(move || {
            let worker = workers.lock().expect("poisoned").recv().expect("stopped");
            worker.scan(height, notif);
            parking.send(worker).expect("closed");
        });
    }
    pub fn scan_range(&mut self, range: RangeInclusive<u64>) -> mpsc::Receiver<Notification> {
        let (sender, receiver) = mpsc::channel();
        let workers = self.workers.clone();
        let parking = self.parking.clone();
        std::thread::spawn(move || {
            for height in range {
                let notif = sender.clone();
                Self::scan(workers.clone(), parking.clone(), height, notif);
            }
        });
        receiver
    }
}
