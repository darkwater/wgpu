use std::sync::Arc;

use parking_lot::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use wgt::Backend;

use crate::{
    id::Id,
    identity::IdentityManager,
    resource::Resource,
    storage::{Element, InvalidId, Storage},
};

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RegistryReport {
    pub num_allocated: usize,
    pub num_kept_from_user: usize,
    pub num_released_from_user: usize,
    pub num_error: usize,
    pub element_size: usize,
}

impl RegistryReport {
    pub fn is_empty(&self) -> bool {
        self.num_allocated + self.num_kept_from_user == 0
    }
}

/// Registry is the primary holder of each resource type
/// Every resource is now arcanized so the last arc released
/// will in the end free the memory and release the inner raw resource
///
/// Registry act as the main entry point to keep resource alive
/// when created and released from user land code
///
/// A resource may still be alive when released from user land code
/// if it's used in active submission or anyway kept alive from
/// any other dependent resource
///
#[derive(Debug)]
pub struct Registry<T: Resource> {
    identity: Arc<IdentityManager<T::Marker>>,
    storage: RwLock<Storage<T>>,
    backend: Backend,
}

impl<T: Resource> Registry<T> {
    pub(crate) fn new(backend: Backend) -> Self {
        Self {
            identity: Arc::new(IdentityManager::new()),
            storage: RwLock::new(Storage::new()),
            backend,
        }
    }

    pub(crate) fn without_backend() -> Self {
        Self::new(Backend::Empty)
    }
}

#[must_use]
pub(crate) struct FutureId<'a, T: Resource> {
    id: Id<T::Marker>,
    data: &'a RwLock<Storage<T>>,
}

impl<T: Resource> FutureId<'_, T> {
    #[allow(dead_code)]
    pub fn id(&self) -> Id<T::Marker> {
        self.id
    }

    pub fn into_id(self) -> Id<T::Marker> {
        self.id
    }

    pub fn init(&self, mut value: T) -> Arc<T> {
        value.as_info_mut().set_id(self.id);
        Arc::new(value)
    }

    pub fn init_in_place(&self, mut value: Arc<T>) -> Arc<T> {
        Arc::get_mut(&mut value)
            .unwrap()
            .as_info_mut()
            .set_id(self.id);
        value
    }

    /// Assign a new resource to this ID.
    ///
    /// Registers it with the registry, and fills out the resource info.
    pub fn assign(self, value: Arc<T>) -> (Id<T::Marker>, Arc<T>) {
        let mut data = self.data.write();
        data.insert(self.id, self.init_in_place(value));
        (self.id, data.get(self.id).unwrap().clone())
    }

    /// Assign an existing resource to a new ID.
    ///
    /// Registers it with the registry.
    ///
    /// This _will_ leak the ID, and it will not be recycled again.
    /// See https://github.com/gfx-rs/wgpu/issues/4912.
    pub fn assign_existing(self, value: &Arc<T>) -> Id<T::Marker> {
        let mut data = self.data.write();
        debug_assert!(!data.contains(self.id));
        data.insert(self.id, value.clone());
        self.id
    }

    pub fn assign_error(self, label: &str) -> Id<T::Marker> {
        self.data.write().insert_error(self.id, label);
        self.id
    }
}

impl<T: Resource> Registry<T> {
    pub(crate) fn prepare(&self, id_in: Option<Id<T::Marker>>) -> FutureId<T> {
        FutureId {
            id: match id_in {
                Some(id_in) => {
                    self.identity.mark_as_used(id_in);
                    id_in
                }
                None => self.identity.process(self.backend),
            },
            data: &self.storage,
        }
    }

    pub(crate) fn request(&self) -> FutureId<T> {
        FutureId {
            id: self.identity.process(self.backend),
            data: &self.storage,
        }
    }
    pub(crate) fn try_get(&self, id: Id<T::Marker>) -> Result<Option<Arc<T>>, InvalidId> {
        self.read().try_get(id).map(|o| o.cloned())
    }
    pub(crate) fn get(&self, id: Id<T::Marker>) -> Result<Arc<T>, InvalidId> {
        self.read().get_owned(id)
    }
    pub(crate) fn read<'a>(&'a self) -> RwLockReadGuard<'a, Storage<T>> {
        self.storage.read()
    }
    pub(crate) fn write<'a>(&'a self) -> RwLockWriteGuard<'a, Storage<T>> {
        self.storage.write()
    }
    pub fn unregister_locked(&self, id: Id<T::Marker>, storage: &mut Storage<T>) -> Option<Arc<T>> {
        self.identity.free(id);
        storage.remove(id)
    }
    pub fn force_replace(&self, id: Id<T::Marker>, mut value: T) {
        let mut storage = self.storage.write();
        value.as_info_mut().set_id(id);
        storage.force_replace(id, value)
    }
    pub fn force_replace_with_error(&self, id: Id<T::Marker>, label: &str) {
        let mut storage = self.storage.write();
        storage.remove(id);
        storage.insert_error(id, label);
    }
    pub(crate) fn unregister(&self, id: Id<T::Marker>) -> Option<Arc<T>> {
        self.identity.free(id);
        let value = self.storage.write().remove(id);
        //Returning None is legal if it's an error ID
        value
    }

    pub fn label_for_resource(&self, id: Id<T::Marker>) -> String {
        let guard = self.storage.read();

        let type_name = guard.kind();
        match guard.get(id) {
            Ok(res) => {
                let label = res.label();
                if label.is_empty() {
                    format!("<{}-{:?}>", type_name, id.unzip())
                } else {
                    label
                }
            }
            Err(_) => format!(
                "<Invalid-{} label={}>",
                type_name,
                guard.label_for_invalid_id(id)
            ),
        }
    }

    pub(crate) fn generate_report(&self) -> RegistryReport {
        let storage = self.storage.read();
        let mut report = RegistryReport {
            element_size: std::mem::size_of::<T>(),
            ..Default::default()
        };
        report.num_allocated = self.identity.values.lock().count();
        for element in storage.map.iter() {
            match *element {
                Element::Occupied(..) => report.num_kept_from_user += 1,
                Element::Vacant => report.num_released_from_user += 1,
                Element::Error(..) => report.num_error += 1,
            }
        }
        report
    }
}
