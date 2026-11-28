use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(feature = "cir_trace")]
use std::sync::OnceLock;

/// Lock-protected shared variable cell (zero synthetic Mutex in MIR).
#[allow(dead_code)]
pub struct SyncVar<T> {
    inner: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SyncVar<T> {}
unsafe impl<T: Send> Send for SyncVar<T> {}

#[allow(dead_code)]
impl<T: Clone> SyncVar<T> {
    pub fn new(v: T) -> Self {
        SyncVar {
            inner: UnsafeCell::new(v),
        }
    }
    pub fn read(&self) -> T {
        unsafe { (*self.inner.get()).clone() }
    }
    pub fn write(&self, v: T) {
        unsafe {
            *self.inner.get() = v;
        }
    }
}

thread_local! {
    static TAG: std::cell::RefCell<String> = std::cell::RefCell::new("t0".to_string());
}

#[allow(dead_code)]
pub fn tag_str() -> String {
    TAG.with(|t| t.borrow().clone())
}

#[allow(dead_code)]
pub fn set_tag(tag: &str) {
    TAG.with(|t| *t.borrow_mut() = tag.to_string());
}

#[cfg(feature = "cir_trace")]
static EVENTS: OnceLock<Mutex<Vec<(String, String, String, String)>>> = OnceLock::new();

/// Record an operation-bound event: (tag, op, resource, sid).
#[allow(dead_code)]
pub fn record(tag: &str, op: &str, resource: &str, sid: &str) {
    #[cfg(feature = "cir_trace")]
    {
        let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
        m.lock().unwrap().push((tag.to_string(), op.to_string(),
                                resource.to_string(), sid.to_string()));
    }
    #[cfg(not(feature = "cir_trace"))]
    {
        let _ = (tag, op, resource, sid);
    }
}

/// Legacy standalone annotation (disabled in codegen v2; kept for tooling).
#[allow(dead_code)]
pub fn ev(tag: &str, sid: &str) {
    record(tag, "ev", "", sid);
}

#[allow(dead_code)]
pub fn finish() {
    #[cfg(feature = "cir_trace")]
    {
        if let Ok(path) = std::env::var("CIR_TRACE_OUT") {
            let m = EVENTS.get_or_init(|| Mutex::new(Vec::new()));
            let guard = m.lock().unwrap();
            let mut out = String::new();
            for (t, op, r, s) in guard.iter() {
                out.push_str(&format!(
                    "{{\"t\":\"{}\",\"sid\":\"{}\",\"op\":\"{}\",\"r\":\"{}\"}}\n",
                    t, s, op, r));
            }
            let _ = std::fs::write(path, out);
        }
    }
}

// ---- operation-bound helpers -------------------------------------------

pub fn lock<'a, T>(m: &'a Mutex<T>, tag: &str, resource: &str, sid: &str)
    -> std::sync::MutexGuard<'a, T> {
    let g = m.lock().unwrap();
    record(tag, "mutex_lock", resource, sid);
    g
}

pub fn unlock<T>(g: Option<std::sync::MutexGuard<'_, T>>, tag: &str,
                 resource: &str, sid: &str) {
    record(tag, "mutex_unlock", resource, sid);
    drop(g);
}

#[allow(dead_code)]
pub fn rwlock_read_lock<'a, T>(l: &'a RwLock<T>, tag: &str, resource: &str, sid: &str)
    -> RwLockReadGuard<'a, T> {
    let g = l.read().unwrap();
    record(tag, "rwlock_read_lock", resource, sid);
    g
}

#[allow(dead_code)]
pub fn rwlock_read_unlock<T>(g: Option<RwLockReadGuard<'_, T>>, tag: &str,
                             resource: &str, sid: &str) {
    record(tag, "rwlock_read_unlock", resource, sid);
    drop(g);
}

#[allow(dead_code)]
pub fn rwlock_write_lock<'a, T>(l: &'a RwLock<T>, tag: &str, resource: &str, sid: &str)
    -> RwLockWriteGuard<'a, T> {
    let g = l.write().unwrap();
    record(tag, "rwlock_write_lock", resource, sid);
    g
}

#[allow(dead_code)]
pub fn rwlock_write_unlock<T>(g: Option<RwLockWriteGuard<'_, T>>, tag: &str,
                              resource: &str, sid: &str) {
    record(tag, "rwlock_write_unlock", resource, sid);
    drop(g);
}

pub fn condvar_wait<'a, T>(cv: &Condvar, g: std::sync::MutexGuard<'a, T>,
                           tag: &str, resource: &str, sid: &str)
    -> std::sync::MutexGuard<'a, T> {
    let g = cv.wait(g).unwrap();
    record(tag, "condvar_wait", resource, sid);
    g
}

pub fn notify_one(cv: &Condvar, tag: &str, resource: &str, sid: &str) {
    record(tag, "condvar_notify", resource, sid);
    cv.notify_one();
}

pub fn notify_all(cv: &Condvar, tag: &str, resource: &str, sid: &str) {
    record(tag, "condvar_notify_all", resource, sid);
    cv.notify_all();
}

pub fn scope(tag: &str, sid: &str) {
    record(tag, "scope", "", sid);
}

pub fn spawn<F>(tag: &str, child: &str, sid: &str, f: F) -> std::thread::JoinHandle<()>
where F: FnOnce() + Send + 'static {
    record(tag, "spawn", child, sid);
    std::thread::spawn(f)
}

pub fn join(h: std::thread::JoinHandle<()>, tag: &str, child: &str, sid: &str) {
    record(tag, "join", child, sid);
    h.join().unwrap();
}

pub struct Semaphore {
    count: Mutex<i64>,
    cv: Condvar,
}

impl Semaphore {
    pub fn new(n: i64) -> Arc<Self> {
        Arc::new(Semaphore { count: Mutex::new(n), cv: Condvar::new() })
    }
    pub fn acquire(&self, n: i64) {
        let mut c = self.count.lock().unwrap();
        while *c < n {
            c = self.cv.wait(c).unwrap();
        }
        *c -= n;
    }
    pub fn release(&self, n: i64) {
        let mut c = self.count.lock().unwrap();
        *c += n;
        self.cv.notify_all();
    }
    pub fn acquire_sid(&self, n: i64, tag: &str, resource: &str, sid: &str) {
        self.acquire(n);
        record(tag, "sem_acquire", resource, sid);
    }
    pub fn release_sid(&self, n: i64, tag: &str, resource: &str, sid: &str) {
        record(tag, "sem_release", resource, sid);
        self.release(n);
    }
}

#[allow(dead_code)]
pub struct Channel<T> {
    buffer: Mutex<VecDeque<T>>,
    cap: usize,
    send_cv: Condvar,
    recv_cv: Condvar,
}

impl<T: Send> Channel<T> {
    pub fn new(cap: usize) -> Arc<Self> {
        Arc::new(Channel {
            buffer: Mutex::new(VecDeque::new()),
            cap,
            send_cv: Condvar::new(),
            recv_cv: Condvar::new(),
        })
    }
    pub fn send(&self, v: T) {
        let mut b = self.buffer.lock().unwrap();
        while self.cap != 0 && b.len() >= self.cap {
            b = self.send_cv.wait(b).unwrap();
        }
        if self.cap == 0 {
            b.push_back(v);
            self.recv_cv.notify_one();
            while !b.is_empty() {
                b = self.send_cv.wait(b).unwrap();
            }
        } else {
            b.push_back(v);
            self.recv_cv.notify_one();
        }
    }
    pub fn recv(&self) -> T {
        let mut b = self.buffer.lock().unwrap();
        while b.is_empty() {
            b = self.recv_cv.wait(b).unwrap();
        }
        let v = b.pop_front().unwrap();
        self.send_cv.notify_one();
        v
    }
    pub fn send_sid(&self, v: T, tag: &str, resource: &str, sid: &str) {
        record(tag, "channel_send", resource, sid);
        self.send(v);
    }
    pub fn recv_sid(&self, tag: &str, resource: &str, sid: &str) -> T {
        record(tag, "channel_recv", resource, sid);
        self.recv()
    }
}
