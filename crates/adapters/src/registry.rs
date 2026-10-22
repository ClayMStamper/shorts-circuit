//! Which adapter serves which platform.

use std::collections::HashMap;
use std::sync::Arc;

use shorts_domain::Platform;

use crate::contract::ErasedAdapter;

/// Adapters by platform. Platforms without an adapter are served by the manual handoff.
#[derive(Default, Clone)]
pub struct AdapterRegistry {
    adapters: HashMap<Platform, Arc<dyn ErasedAdapter>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, adapter: Arc<dyn ErasedAdapter>) {
        self.adapters.insert(adapter.platform(), adapter);
    }

    pub fn get(&self, platform: Platform) -> Option<Arc<dyn ErasedAdapter>> {
        self.adapters.get(&platform).cloned()
    }
}
