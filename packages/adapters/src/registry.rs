use crate::HarnessAdapter;
use reins_core::HarnessProfile;
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("no adapter factory registered for harness id '{0}'")]
    UnknownHarness(String),
}

pub trait AdapterFactory: Send + Sync {
    fn id(&self) -> &'static str;
    fn create(&self, profile: HarnessProfile) -> Box<dyn HarnessAdapter>;
}

#[derive(Default)]
pub struct AdapterRegistry {
    factories: HashMap<&'static str, Box<dyn AdapterFactory>>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, factory: Box<dyn AdapterFactory>) {
        self.factories.insert(factory.id(), factory);
    }

    pub fn build(
        &self,
        id: &str,
        profile: HarnessProfile,
    ) -> Result<Box<dyn HarnessAdapter>, RegistryError> {
        self.factories
            .get(id)
            .map(|f| f.create(profile))
            .ok_or_else(|| RegistryError::UnknownHarness(id.to_string()))
    }

    pub fn registered_ids(&self) -> Vec<&'static str> {
        self.factories.keys().copied().collect()
    }
}
