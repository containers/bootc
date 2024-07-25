use std::env;
use std::ops::Deref;

use anyhow::Result;
use cap_std_ext::cap_std::fs::Dir;
use clap::ValueEnum;

use ostree_ext::container::OstreeImageReference;
use ostree_ext::keyfileext::KeyFileExt;
use ostree_ext::ostree;
use ostree_ext::sysroot::SysrootLock;

use crate::spec::ImageStatus;

mod ostree_container;

pub(crate) struct Storage {
    pub sysroot: SysrootLock,
    #[allow(dead_code)]
    pub imgstore: crate::imgstorage::Storage,
    pub store: Box<dyn ContainerImageStoreImpl>,
}

#[derive(Default)]
pub(crate) struct CachedImageStatus {
    pub image: Option<ImageStatus>,
    pub cached_update: Option<ImageStatus>,
}

pub(crate) trait ContainerImageStore {
    fn store(&self) -> Result<Option<Box<dyn ContainerImageStoreImpl>>>;
}

pub(crate) trait ContainerImageStoreImpl {
    fn spec(&self) -> crate::spec::Store;

    fn imagestatus(
        &self,
        sysroot: &SysrootLock,
        deployment: &ostree::Deployment,
        image: OstreeImageReference,
    ) -> Result<CachedImageStatus>;
}

impl Deref for Storage {
    type Target = SysrootLock;

    fn deref(&self) -> &Self::Target {
        &self.sysroot
    }
}

impl Storage {
    pub fn new(sysroot: SysrootLock, run: &Dir) -> Result<Self> {
        let store = match env::var("BOOTC_STORAGE") {
            Ok(val) => crate::spec::Store::from_str(&val, true).unwrap_or_else(|_| {
                let default = crate::spec::Store::default();
                tracing::warn!("Unknown BOOTC_STORAGE option {val}, falling back to {default:?}");
                default
            }),
            Err(_) => crate::spec::Store::default(),
        };

        let sysroot_dir = Dir::reopen_dir(&crate::utils::sysroot_fd(&sysroot))?;
        let imgstore = crate::imgstorage::Storage::open(&sysroot_dir, run)?;

        let store = load(store);

        Ok(Self {
            sysroot,
            store,
            imgstore,
        })
    }
}

impl ContainerImageStore for ostree::Deployment {
    fn store<'a>(&self) -> Result<Option<Box<dyn ContainerImageStoreImpl>>> {
        if let Some(origin) = self.origin().as_ref() {
            if let Some(store) = origin.optional_string("bootc", "backend")? {
                let store =
                    crate::spec::Store::from_str(&store, true).map_err(anyhow::Error::msg)?;
                Ok(Some(load(store)))
            } else {
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }
}

pub(crate) fn load(ty: crate::spec::Store) -> Box<dyn ContainerImageStoreImpl> {
    match ty {
        crate::spec::Store::OstreeContainer => Box::new(ostree_container::OstreeContainerStore),
    }
}
