//! Metadata about the source of an object: a component or package.
//!
//! This is used to help split up containers into distinct layers.

use indexmap::IndexMap;
use std::borrow::Borrow;
use std::collections::HashSet;
use std::hash::Hash;
use std::rc::Rc;

use serde::{Deserialize, Serialize, Serializer};

mod rcstr_serialize {
    use serde::Deserializer;

    use super::*;

    pub(crate) fn serialize<S>(v: &Rc<str>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(v)
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<Rc<str>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = String::deserialize(deserializer)?;
        Ok(Rc::from(v.into_boxed_str()))
    }
}

/// Identifier for content (e.g. package/layer).  Not necessarily human readable.
/// For example in RPMs, this may be a full "NEVRA" i.e. name-epoch:version-release.architecture e.g. kernel-6.2-2.fc38.aarch64
/// But that's not strictly required as this string should only live in memory and not be persisted.
pub type ContentID = Rc<str>;

/// Metadata about a component/package.
#[derive(Debug, Eq, Deserialize, Serialize)]
pub struct ObjectSourceMeta {
    /// Unique identifier, does not need to be human readable, but can be.
    #[serde(with = "rcstr_serialize")]
    pub identifier: ContentID,
    /// Just the name of the package (no version), needs to be human readable.
    #[serde(with = "rcstr_serialize")]
    pub name: Rc<str>,
    /// Identifier for the *source* of this content; for example, if multiple binary
    /// packages derive from a single git repository or source package.
    #[serde(with = "rcstr_serialize")]
    pub srcid: Rc<str>,
    /// Unitless, relative offset of last change time.
    /// One suggested way to generate this number is to have it be in units of hours or days
    /// since the earliest changed item.
    pub change_time_offset: u32,
    /// Change frequency
    pub change_frequency: u32,
}

impl PartialEq for ObjectSourceMeta {
    fn eq(&self, other: &Self) -> bool {
        *self.identifier == *other.identifier
    }
}

impl Hash for ObjectSourceMeta {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.identifier.hash(state);
    }
}

impl Borrow<str> for ObjectSourceMeta {
    fn borrow(&self) -> &str {
        &self.identifier
    }
}

/// Maps from e.g. "bash" or "kernel" to metadata about that content
pub type ObjectMetaSet = HashSet<ObjectSourceMeta>;

/// Maps from an ostree content object digest to the `ContentSet` key.
pub type ObjectMetaMap = IndexMap<String, ContentID>;

/// Grouping of metadata about an object.
#[derive(Debug, Default)]
pub struct ObjectMeta {
    /// The set of object sources with their metadata.
    pub set: ObjectMetaSet,
    /// Mapping from content object to source.
    pub map: ObjectMetaMap,
}
