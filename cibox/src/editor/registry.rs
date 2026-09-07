use super::config::EditorPreset;
use std::sync::Arc;

/// Global registry of all presets
///
/// Uses a simple Vec for storage since we have a small number of presets (~5).
/// Linear search is acceptable for this scale and simplifies the implementation.
pub struct PresetRegistry {
    presets: Vec<Arc<dyn EditorPreset>>,
}

impl Default for PresetRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PresetRegistry {
    pub fn new() -> Self {
        Self {
            presets: Vec::new(),
        }
    }

    pub fn register(&mut self, preset: Arc<dyn EditorPreset>) {
        self.presets.push(preset);
    }

    pub fn get(&self, id: &str) -> Option<&Arc<dyn EditorPreset>> {
        self.presets.iter().find(|p| p.preset_id() == id)
    }

    pub fn all(&self) -> Vec<&Arc<dyn EditorPreset>> {
        self.presets.iter().collect()
    }
}

macro_rules! generate_build_registry {
    ($(($variant:ident, $ty:path, $display:literal)),+ $(,)?) => {
        /// Build the global preset registry
        pub fn build_registry() -> PresetRegistry {
            let mut registry = PresetRegistry::new();
            $(registry.register(Arc::new(<$ty>::default()));)+
            registry
        }
    };
}
crate::presets::with_presets!(generate_build_registry);
