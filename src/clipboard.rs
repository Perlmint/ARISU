use std::sync::{Arc, RwLock};

use ironrdp::{
    cliprdr::backend::{CliprdrBackend, CliprdrBackendFactory},
    server::{CliprdrServerFactory, ServerEvent, ServerEventSender},
};
use ironrdp_cliprdr_native::StubCliprdrBackend;
use tokio::sync::mpsc::UnboundedSender;

pub struct StubCliprdrServerFactory {
    inner: Arc<RwLock<Option<UnboundedSender<ServerEvent>>>>,
}

impl StubCliprdrServerFactory {
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl CliprdrBackendFactory for StubCliprdrServerFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(StubCliprdrBackend::new())
    }
}

impl ServerEventSender for StubCliprdrServerFactory {
    fn set_sender(&mut self, sender: UnboundedSender<ServerEvent>) {
        let mut inner = self.inner.write().expect("Failed to retreive write lock");
        *inner = Some(sender);
    }
}

impl CliprdrServerFactory for StubCliprdrServerFactory {}
