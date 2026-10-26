use super::{Error, Result};
use crate::adapter_api::{ABI_VERSION, MAX_MESSAGE_BYTES, PluginInput, PluginOutput, PluginResult};
use std::sync::Arc;
use wasmi::{
    Config, Engine, Linker, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};

pub(crate) const MAX_WASM_BYTES: usize = 8 * 1024 * 1024;
const FUEL_PER_CALL: u64 = 100_000_000;
const MEMORY_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct Plugin {
    engine: Engine,
    module: Arc<Module>,
}
impl Plugin {
    pub fn load(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_WASM_BYTES {
            return Err(Error::new("adapter module exceeds 8 MiB"));
        }
        let mut config = Config::default();
        config.consume_fuel(true);
        let engine = Engine::new(&config);
        let module = Module::new(&engine, bytes)?;
        if module.imports().next().is_some() {
            return Err(Error::new(
                "adapter plugins must not import host functions or WASI",
            ));
        }
        let plugin = Self {
            engine,
            module: Arc::new(module),
        };
        // Validate ABI, exports, and memory before accepting a plugin.
        plugin.session()?;
        Ok(plugin)
    }
    #[cfg(test)]
    pub fn weak_module(&self) -> std::sync::Weak<Module> {
        Arc::downgrade(&self.module)
    }
    pub fn session(&self) -> Result<Session> {
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES)
            .table_elements(10_000)
            .instances(1)
            .memories(1)
            .tables(1)
            .trap_on_grow_failure(true)
            .build();
        let mut store = Store::new(&self.engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(FUEL_PER_CALL)?;
        let instance = Linker::new(&self.engine)
            .instantiate(&mut store, &self.module)?
            .start(&mut store)?;
        let abi = instance
            .get_typed_func::<(), u32>(&store, "adapter_abi_version")?
            .call(&mut store, ())?;
        if abi != ABI_VERSION {
            return Err(Error::new(format!(
                "unsupported adapter ABI {abi}; expected {ABI_VERSION}"
            )));
        }
        let memory = instance
            .get_memory(&store, "memory")
            .ok_or_else(|| Error::new("adapter does not export memory"))?;
        let alloc = instance.get_typed_func(&store, "adapter_alloc")?;
        let free = instance.get_typed_func(&store, "adapter_free")?;
        let call = instance.get_typed_func(&store, "adapter_call")?;
        Ok(Session {
            store,
            memory,
            alloc,
            free,
            call,
        })
    }
}

pub(crate) struct Session {
    store: Store<StoreLimits>,
    memory: Memory,
    alloc: TypedFunc<u32, u32>,
    free: TypedFunc<(u32, u32), ()>,
    call: TypedFunc<(u32, u32), u64>,
}
impl Session {
    pub fn invoke(&mut self, input: &PluginInput) -> Result<PluginOutput> {
        let bytes = serde_json::to_vec(input)?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(Error::new("adapter input exceeds 8 MiB"));
        }
        self.store.set_fuel(FUEL_PER_CALL)?;
        let length = bytes.len() as u32;
        let pointer = self.alloc.call(&mut self.store, length)?;
        self.memory
            .write(&mut self.store, pointer as usize, &bytes)
            .map_err(Error::new)?;
        let output = self.call.call(&mut self.store, (pointer, length))?;
        self.free.call(&mut self.store, (pointer, length))?;
        let output_pointer = (output >> 32) as u32;
        let output_length = output as u32;
        if output_length as usize > MAX_MESSAGE_BYTES {
            return Err(Error::new("adapter output exceeds 8 MiB"));
        }
        let mut bytes = vec![0; output_length as usize];
        self.memory
            .read(&self.store, output_pointer as usize, &mut bytes)
            .map_err(Error::new)?;
        self.free
            .call(&mut self.store, (output_pointer, output_length))?;
        let result: PluginResult = serde_json::from_slice(&bytes)?;
        result.map_err(Error::new)
    }
}
