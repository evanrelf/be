use crate::cache::Cache;
use std::sync::OnceLock;
use tokio::sync::Semaphore;

pub struct Context {
    pub cache: Cache,
    pub file_permits: Semaphore,
    pub process_permits: Semaphore,
}

pub static CONTEXT: OnceLock<Context> = OnceLock::new();

pub fn cx() -> &'static Context {
    CONTEXT.get().unwrap()
}
