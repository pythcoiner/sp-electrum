use std::{
    ops::RangeInclusive,
    panic::UnwindSafe,
    sync::{
        atomic::AtomicBool,
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
    _contexts_parking: mpsc::Sender<Context>,
    _context_pools: Arc<Mutex<mpsc::Receiver<Context>>>,
}

type Task = Box<dyn FnOnce(Context) -> Context + Send + UnwindSafe + 'static>;

impl ThreadPool {
    pub fn new(size: usize, context: &Context) -> Self {
        let (tx, rx) = mpsc::channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));

        let mut workers = Vec::with_capacity(size);

        let (contexts_parking, conytexts_pool) = mpsc::channel();
        let contexts_pool = Arc::new(Mutex::new(conytexts_pool));

        for _ in 0..size {
            let rx = Arc::clone(&rx);
            let contexts_parking = contexts_parking.clone();
            contexts_parking.send(context.clone()).expect("poisoned");
            let contexts_pool = contexts_pool.clone();

            workers.push(
                std::thread::Builder::new()
                    // TODO: verify stack size
                    .stack_size(1024)
                    .spawn(move || loop {
                        let sp_receiver = contexts_pool
                            .lock()
                            .expect("poisoned")
                            .recv()
                            .expect("closed");
                        let task: Task = match rx.lock().expect("poisoned").recv() {
                            Ok(task) => task,
                            Err(_) => {
                                // TODO: log
                                contexts_parking.send(sp_receiver).expect("closed");
                                break;
                            }
                        };
                        // // if std::panic::catch_unwind(task).is_err() {
                        // //     // TODO: log
                        // }
                        let recv = task(sp_receiver);
                        contexts_parking.send(recv).expect("closed");
                    })
                    .expect("must not fail"),
            );
        }
        ThreadPool {
            workers,
            sender: tx,
            _contexts_parking: contexts_parking,
            _context_pools: contexts_pool,
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
    stop: Arc<AtomicBool>,
}

impl Worker {
    pub fn new(threads: usize, context: Context, stop: Arc<AtomicBool>) -> Self {
        let pool = ThreadPool::new(threads, &context);
        Self {
            pool,
            context: context.clone(),
            stop,
        }
    }
    pub fn scan(&mut self, height: u64, notif: mpsc::Sender<Notification>) {
        notif
            .send(Notification::StartBlock(height))
            .expect("closed");
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
    tasks: Arc<Mutex<mpsc::Receiver<u64>>>,
    sender: mpsc::Sender<u64>,
    notif: Option<mpsc::Receiver<Notification>>,
    notif_sender: mpsc::Sender<Notification>,
    stop: Arc<AtomicBool>,
}

impl WorkerPool {
    pub fn new(workers: usize, threads: usize, context: &Context) -> Self {
        let count = workers;
        let (sender, tasks) = mpsc::channel();
        let (notif_sender, notif) = mpsc::channel();
        let tasks = Arc::new(Mutex::new(tasks));
        let stop = Arc::new(AtomicBool::new(false));

        for _ in 0..count {
            let tasks = tasks.clone();
            let mut worker = Worker::new(threads, context.clone(), stop.clone());
            let notif = notif_sender.clone();
            std::thread::Builder::new()
                // TODO: verify stack size
                .stack_size(1024)
                .spawn(move || loop {
                    if worker.stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let height = tasks.lock().expect("poisoned").recv().expect("closed");
                    worker.scan(height, notif.clone());
                })
                .expect("fail to start worker thread");
        }
        Self {
            tasks,
            sender,
            notif: Some(notif),
            notif_sender,
            stop,
        }
    }
    pub fn stop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut aborted_sent = false;
        while let Ok(height) = self.tasks.lock().expect("poisoned").try_recv() {
            if !aborted_sent {
                self.notif_sender
                    .send(Notification::Aborted(height))
                    .expect("closed");
                aborted_sent = true;
            } else {
                // drop remaining heights
            }
        }
    }
    pub fn start(&mut self, range: RangeInclusive<u64>) -> mpsc::Receiver<Notification> {
        if self.notif.is_none() {
            panic!("can be start only once")
        }
        let task_sender = self.sender.clone();
        let notif_sender = self.notif_sender.clone();
        std::thread::spawn(move || {
            notif_sender
                .send(Notification::StartScan(*range.start(), *range.end()))
                .expect("closed");
            for height in range {
                task_sender.send(height).expect("closed");
            }
        });
        self.notif.take().expect("checked")
    }
}
