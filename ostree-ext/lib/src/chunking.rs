//! Split an OSTree commit into separate chunks

// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::borrow::{Borrow, Cow};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write;
use std::num::NonZeroU32;
use std::rc::Rc;

use crate::objectsource::{ContentID, ObjectMeta, ObjectMetaMap, ObjectSourceMeta};
use crate::objgv::*;
use anyhow::{anyhow, Result};
use camino::Utf8PathBuf;
use gvariant::aligned_bytes::TryAsAligned;
use gvariant::{Marker, Structure};
use ostree::{gio, glib};
use serde::{Deserialize, Serialize};

/// Maximum number of layers (chunks) we will use.
// We take half the limit of 128.
// https://github.com/ostreedev/ostree-rs-ext/issues/69
pub(crate) const MAX_CHUNKS: u32 = 64;

type RcStr = Rc<str>;
pub(crate) type ChunkMapping = BTreeMap<RcStr, (u64, Vec<Utf8PathBuf>)>;

#[derive(Debug, Default)]
pub(crate) struct Chunk {
    pub(crate) name: String,
    pub(crate) content: ChunkMapping,
    pub(crate) size: u64,
}

#[derive(Debug, Deserialize, Serialize)]
/// Object metadata, but with additional size data
pub struct ObjectSourceMetaSized {
    /// The original metadata
    #[serde(flatten)]
    meta: ObjectSourceMeta,
    /// Total size of associated objects
    size: u64,
}

/// Extend content source metadata with sizes.
#[derive(Debug)]
pub struct ObjectMetaSized {
    /// Mapping from content object to source.
    pub map: ObjectMetaMap,
    /// Computed sizes of each content source
    pub sizes: Vec<ObjectSourceMetaSized>,
}

impl ObjectMetaSized {
    /// Given object metadata and a repo, compute the size of each content source.
    pub fn compute_sizes(repo: &ostree::Repo, meta: ObjectMeta) -> Result<ObjectMetaSized> {
        let cancellable = gio::NONE_CANCELLABLE;
        // Destructure into component parts; we'll create the version with sizes
        let map = meta.map;
        let mut set = meta.set;
        // Maps content id -> total size of associated objects
        let mut sizes = HashMap::<&str, u64>::new();
        // Populate two mappings above, iterating over the object -> contentid mapping
        for (checksum, contentid) in map.iter() {
            let finfo = repo.query_file(checksum, cancellable)?.0;
            let sz = sizes.entry(contentid).or_default();
            *sz += finfo.size() as u64;
        }
        // Combine data from sizes and the content mapping.
        let sized: Result<Vec<_>> = sizes
            .into_iter()
            .map(|(id, size)| -> Result<ObjectSourceMetaSized> {
                set.take(id)
                    .ok_or_else(|| anyhow!("Failed to find {} in content set", id))
                    .map(|meta| ObjectSourceMetaSized { meta, size })
            })
            .collect();
        let mut sizes = sized?;
        sizes.sort_by(|a, b| b.size.cmp(&a.size));
        Ok(ObjectMetaSized { map, sizes })
    }
}

/// How to split up an ostree commit into "chunks" - designed to map to container image layers.
#[derive(Debug, Default)]
pub struct Chunking {
    pub(crate) metadata_size: u64,
    pub(crate) remainder: Chunk,
    pub(crate) chunks: Vec<Chunk>,

    pub(crate) max: u32,

    processed_mapping: bool,
    /// Number of components (e.g. packages) provided originally
    pub(crate) n_provided_components: u32,
    /// The above, but only ones with non-zero size
    pub(crate) n_sized_components: u32,
}

#[derive(Default)]
struct Generation {
    path: Utf8PathBuf,
    metadata_size: u64,
    dirtree_found: BTreeSet<RcStr>,
    dirmeta_found: BTreeSet<RcStr>,
}

fn push_dirmeta(repo: &ostree::Repo, gen: &mut Generation, checksum: &str) -> Result<()> {
    if gen.dirtree_found.contains(checksum) {
        return Ok(());
    }
    let checksum = RcStr::from(checksum);
    gen.dirmeta_found.insert(RcStr::clone(&checksum));
    let child_v = repo.load_variant(ostree::ObjectType::DirMeta, checksum.borrow())?;
    gen.metadata_size += child_v.data_as_bytes().as_ref().len() as u64;
    Ok(())
}

fn push_dirtree(
    repo: &ostree::Repo,
    gen: &mut Generation,
    checksum: &str,
) -> Result<glib::Variant> {
    let child_v = repo.load_variant(ostree::ObjectType::DirTree, checksum)?;
    if !gen.dirtree_found.contains(checksum) {
        gen.metadata_size += child_v.data_as_bytes().as_ref().len() as u64;
    } else {
        let checksum = RcStr::from(checksum);
        gen.dirtree_found.insert(checksum);
    }
    Ok(child_v)
}

fn generate_chunking_recurse(
    repo: &ostree::Repo,
    gen: &mut Generation,
    chunk: &mut Chunk,
    dt: &glib::Variant,
) -> Result<()> {
    let dt = dt.data_as_bytes();
    let dt = dt.try_as_aligned()?;
    let dt = gv_dirtree!().cast(dt);
    let (files, dirs) = dt.to_tuple();
    // A reusable buffer to avoid heap allocating these
    let mut hexbuf = [0u8; 64];
    for file in files {
        let (name, csum) = file.to_tuple();
        let fpath = gen.path.join(name.to_str());
        hex::encode_to_slice(csum, &mut hexbuf)?;
        let checksum = std::str::from_utf8(&hexbuf)?;
        let meta = repo.query_file(checksum, gio::NONE_CANCELLABLE)?.0;
        let size = meta.size() as u64;
        let entry = chunk.content.entry(RcStr::from(checksum)).or_default();
        entry.0 = size;
        let first = entry.1.is_empty();
        if first {
            chunk.size += size;
        }
        entry.1.push(fpath);
    }
    for item in dirs {
        let (name, contents_csum, meta_csum) = item.to_tuple();
        let name = name.to_str();
        // Extend our current path
        gen.path.push(name);
        hex::encode_to_slice(contents_csum, &mut hexbuf)?;
        let checksum_s = std::str::from_utf8(&hexbuf)?;
        let dirtree_v = push_dirtree(repo, gen, checksum_s)?;
        generate_chunking_recurse(repo, gen, chunk, &dirtree_v)?;
        drop(dirtree_v);
        hex::encode_to_slice(meta_csum, &mut hexbuf)?;
        let checksum_s = std::str::from_utf8(&hexbuf)?;
        push_dirmeta(repo, gen, checksum_s)?;
        // We did a push above, so pop must succeed.
        assert!(gen.path.pop());
    }
    Ok(())
}

impl Chunk {
    fn new(name: &str) -> Self {
        Chunk {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn move_obj(&mut self, dest: &mut Self, checksum: &str) -> bool {
        // In most cases, we expect the object to exist in the source.  However, it's
        // conveneient here to simply ignore objects which were already moved into
        // a chunk.
        if let Some((name, (size, paths))) = self.content.remove_entry(checksum) {
            let v = dest.content.insert(name, (size, paths));
            debug_assert!(v.is_none());
            self.size -= size;
            dest.size += size;
            true
        } else {
            false
        }
    }
}

impl Chunking {
    /// Generate an initial single chunk.
    pub fn new(repo: &ostree::Repo, rev: &str) -> Result<Self> {
        // Find the target commit
        let rev = repo.require_rev(rev)?;

        // Load and parse the commit object
        let (commit_v, _) = repo.load_commit(&rev)?;
        let commit_v = commit_v.data_as_bytes();
        let commit_v = commit_v.try_as_aligned()?;
        let commit = gv_commit!().cast(commit_v);
        let commit = commit.to_tuple();

        // Load it all into a single chunk
        let mut gen = Generation {
            path: Utf8PathBuf::from("/"),
            ..Default::default()
        };
        let mut chunk: Chunk = Default::default();

        // Find the root directory tree
        let contents_checksum = &hex::encode(commit.6);
        let contents_v = repo.load_variant(ostree::ObjectType::DirTree, contents_checksum)?;
        push_dirtree(repo, &mut gen, contents_checksum)?;
        let meta_checksum = &hex::encode(commit.7);
        push_dirmeta(repo, &mut gen, meta_checksum.as_str())?;

        generate_chunking_recurse(repo, &mut gen, &mut chunk, &contents_v)?;

        let chunking = Chunking {
            metadata_size: gen.metadata_size,
            remainder: chunk,
            ..Default::default()
        };
        Ok(chunking)
    }

    /// Generate a chunking from an object mapping.
    pub fn from_mapping(
        repo: &ostree::Repo,
        rev: &str,
        meta: ObjectMetaSized,
        max_layers: Option<NonZeroU32>,
    ) -> Result<Self> {
        let mut r = Self::new(repo, rev)?;
        r.process_mapping(meta, max_layers)?;
        Ok(r)
    }

    fn remaining(&self) -> u32 {
        self.max.saturating_sub(self.chunks.len() as u32)
    }

    /// Given metadata about which objects are owned by a particular content source,
    /// generate chunks that group together those objects.
    #[allow(clippy::or_fun_call)]
    pub fn process_mapping(
        &mut self,
        meta: ObjectMetaSized,
        max_layers: Option<NonZeroU32>,
    ) -> Result<()> {
        self.max = max_layers
            .unwrap_or(NonZeroU32::new(MAX_CHUNKS).unwrap())
            .get();

        let sizes = &meta.sizes;
        // It doesn't make sense to handle multiple mappings
        assert!(!self.processed_mapping);
        self.processed_mapping = true;
        let remaining = self.remaining();
        if remaining == 0 {
            return Ok(());
        }

        // Reverses `contentmeta.map` i.e. contentid -> Vec<checksum>
        let mut rmap = HashMap::<ContentID, Vec<&String>>::new();
        for (checksum, contentid) in meta.map.iter() {
            rmap.entry(Rc::clone(contentid)).or_default().push(checksum);
        }

        // Safety: Let's assume no one has over 4 billion components.
        self.n_provided_components = meta.sizes.len().try_into().unwrap();
        self.n_sized_components = sizes
            .iter()
            .filter(|v| v.size > 0)
            .count()
            .try_into()
            .unwrap();

        // TODO: Compute bin packing in a better way
        let packing = basic_packing(sizes, NonZeroU32::new(self.max).unwrap());

        for bin in packing.into_iter() {
            let first = bin[0];
            let first_name = &*first.meta.name;
            let name = match bin.len() {
                0 => unreachable!(),
                1 => Cow::Borrowed(first_name),
                2..=5 => {
                    let r = bin.iter().map(|v| &*v.meta.name).fold(
                        String::from(first_name),
                        |mut acc, v| {
                            write!(acc, " and {}", v).unwrap();
                            acc
                        },
                    );
                    Cow::Owned(r)
                }
                n => Cow::Owned(format!("{n} components")),
            };
            let mut chunk = Chunk::new(&*name);
            for szmeta in bin {
                for &obj in rmap.get(&szmeta.meta.identifier).unwrap() {
                    self.remainder.move_obj(&mut chunk, obj.as_str());
                }
            }
            if !chunk.content.is_empty() {
                self.chunks.push(chunk);
            }
        }

        assert_eq!(self.remainder.content.len(), 0);

        Ok(())
    }

    pub(crate) fn take_chunks(&mut self) -> Vec<Chunk> {
        let mut r = Vec::new();
        std::mem::swap(&mut self.chunks, &mut r);
        r
    }

    /// Print information about chunking to standard output.
    pub fn print(&self) {
        println!("Metadata: {}", glib::format_size(self.metadata_size));
        if self.n_provided_components > 0 {
            println!(
                "Components: provided={} sized={}",
                self.n_provided_components, self.n_sized_components
            );
        }
        for (n, chunk) in self.chunks.iter().enumerate() {
            let sz = glib::format_size(chunk.size);
            println!(
                "Chunk {}: \"{}\": objects:{} size:{}",
                n,
                chunk.name,
                chunk.content.len(),
                sz
            );
        }
        if !self.remainder.content.is_empty() {
            let sz = glib::format_size(self.remainder.size);
            println!(
                "Remainder: \"{}\": objects:{} size:{}",
                self.remainder.name,
                self.remainder.content.len(),
                sz
            );
        }
    }
}

type ChunkedComponents<'a> = Vec<&'a ObjectSourceMetaSized>;

fn components_size(components: &[&ObjectSourceMetaSized]) -> u64 {
    components.iter().map(|k| k.size).sum()
}

/// Compute the total size of a packing
#[cfg(test)]
fn packing_size(packing: &[ChunkedComponents]) -> u64 {
    packing.iter().map(|v| components_size(v)).sum()
}

fn sort_packing(packing: &mut [ChunkedComponents]) {
    packing.sort_by(|a, b| {
        let a: u64 = components_size(a);
        let b: u64 = components_size(b);
        b.cmp(&a)
    });
}

/// Given a set of components with size metadata (e.g. boxes of a certain size)
/// and a number of bins (possible container layers) to use, determine which components
/// go in which bin.  This algorithm is pretty simple:
///
/// - order by size
/// - If we have fewer components than bins, we're done
/// - Take the "tail" (all components past maximum), and group by source package
/// - If we have fewer components than bins, we're done
/// - Take the whole tail and group them toether (this is the overly simplistic part)
fn basic_packing(components: &[ObjectSourceMetaSized], bins: NonZeroU32) -> Vec<ChunkedComponents> {
    // let total_size: u64 = components.iter().map(|v| v.size).sum();
    // let avg_size: u64 = total_size / components.len() as u64;
    let mut r = Vec::new();
    // And handle the easy case of enough bins for all components
    // TODO: Possibly try to split off large files?
    if components.len() <= bins.get() as usize {
        r.extend(components.iter().map(|v| vec![v]));
        return r;
    }
    // Create a mutable copy
    let mut components: Vec<_> = components.iter().collect();
    // Iterate over the component tail, folding by source id
    let mut by_src = HashMap::<_, Vec<&ObjectSourceMetaSized>>::new();
    // Take the tail off components, then build up mapping from srcid -> Vec<component>
    for component in components.split_off(bins.get() as usize) {
        by_src
            .entry(&component.meta.srcid)
            .or_default()
            .push(component);
    }
    // Take all the non-tail (largest) components, and append them first
    r.extend(components.into_iter().map(|v| vec![v]));
    // Add the tail
    r.extend(by_src.into_values());
    // And order the new list
    sort_packing(&mut r);
    // It's possible that merging components gave us enough space; if so
    // we're done!
    if r.len() <= bins.get() as usize {
        return r;
    }

    let last = (bins.get().checked_sub(1).unwrap()) as usize;
    // The "tail" is components past our maximum.  For now, we simply group all of that together as a single unit.
    if let Some(tail) = r.drain(last..).reduce(|mut a, b| {
        a.extend(b.into_iter());
        a
    }) {
        r.push(tail);
    }

    assert!(r.len() <= bins.get() as usize);
    r
}

#[cfg(test)]
mod test {
    use super::*;

    const FCOS_CONTENTMETA: &[u8] = include_bytes!("fixtures/fedora-coreos-contentmeta.json.gz");

    #[test]
    fn test_packing_basics() -> Result<()> {
        // null cases
        for v in [1u32, 7].map(|v| NonZeroU32::new(v).unwrap()) {
            assert_eq!(basic_packing(&[], v).len(), 0);
        }
        Ok(())
    }

    #[test]
    fn test_packing_fcos() -> Result<()> {
        let contentmeta: Vec<ObjectSourceMetaSized> =
            serde_json::from_reader(flate2::read::GzDecoder::new(FCOS_CONTENTMETA))?;
        let total_size = contentmeta.iter().map(|v| v.size).sum::<u64>();

        let packing = basic_packing(&contentmeta, NonZeroU32::new(MAX_CHUNKS).unwrap());
        assert!(!contentmeta.is_empty());
        // We should fit into the assigned chunk size
        assert_eq!(packing.len() as u32, MAX_CHUNKS);
        // And verify that the sizes match
        let packed_total_size = packing_size(&packing);
        assert_eq!(total_size, packed_total_size);
        Ok(())
    }
}
