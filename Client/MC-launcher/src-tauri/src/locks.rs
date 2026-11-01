use std::{
    collections::HashSet,
    sync::{Mutex, OnceLock},
};

static INSTANCE_LOCKS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn instance_locks() -> &'static Mutex<HashSet<String>> {
    INSTANCE_LOCKS.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Exclusive guard for long-running operations on an instance.
///
/// The guard is also kept alive for the whole lifetime of a game process, so
/// operations such as renaming, deleting or duplicating a running instance are
/// rejected instead of corrupting its files. These locks are process-local and
/// rely on the single-instance plugin to prevent a second launcher process.
pub(crate) struct InstanceLock {
    name: String,
}

impl InstanceLock {
    pub(crate) fn acquire(name: &str) -> Result<Self, String> {
        let mut locks = instance_locks()
            .lock()
            .map_err(|_| "Не удалось получить блокировку инстанса.".to_string())?;
        if !locks.insert(name.to_string()) {
            return Err(format!(
                "Инстанс «{name}» уже используется (установка, запуск или изменение)."
            ));
        }
        Ok(Self {
            name: name.to_string(),
        })
    }

    pub(crate) fn acquire_all(names: &[&str]) -> Result<Vec<Self>, String> {
        let mut unique = names.to_vec();
        unique.sort_unstable();
        unique.dedup();
        let mut guards = Vec::with_capacity(unique.len());
        for name in unique {
            guards.push(Self::acquire(name)?);
        }
        Ok(guards)
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Ok(mut locks) = instance_locks().lock() {
            locks.remove(&self.name);
        }
    }
}
