//! Extension APIs for working with GVariant.  Not strictly
//! related to ostree, but included here in the interest of
//! avoiding another crate for this.  In the future, some of these
//! may migrate into gtk-rs.

use glib::translate::*;
use glib::ToVariant;
use std::mem::size_of;

/// Create a new GVariant from data.
pub fn variant_new_from_bytes(ty: &str, bytes: glib::Bytes, trusted: bool) -> glib::Variant {
    unsafe {
        let ty = ty.to_glib_none();
        let ty: *const libc::c_char = ty.0;
        let ty = ty as *const glib_sys::GVariantType;
        let bytes = bytes.to_glib_full();
        let v = glib_sys::g_variant_new_from_bytes(ty, bytes, trusted.to_glib());
        glib_sys::g_variant_ref_sink(v);
        from_glib_full(v)
    }
}

/// Get the normal form of a GVariant.
pub fn variant_get_normal_form(v: &glib::Variant) -> glib::Variant {
    unsafe { from_glib_full(glib_sys::g_variant_get_normal_form(v.to_glib_none().0)) }
}

/// Create a normal-form GVariant from raw bytes.
pub fn variant_normal_from_bytes(ty: &str, bytes: glib::Bytes) -> glib::Variant {
    variant_get_normal_form(&variant_new_from_bytes(ty, bytes, false))
}

/// Create a new `ay` GVariant.
pub fn new_variant_bytearray(buf: &[u8]) -> glib::Variant {
    unsafe {
        let r = glib_sys::g_variant_new_fixed_array(
            b"y\0".as_ptr() as *const _,
            buf.as_ptr() as *const _,
            buf.len(),
            size_of::<u8>(),
        );
        glib_sys::g_variant_ref_sink(r);
        from_glib_full(r)
    }
}

/// Create a new GVariant tuple from the provided variants.
pub fn new_variant_tuple<'a>(items: impl IntoIterator<Item = &'a glib::Variant>) -> glib::Variant {
    let v: Vec<_> = items.into_iter().map(|v| v.to_glib_none().0).collect();
    unsafe {
        let r = glib_sys::g_variant_new_tuple(v.as_ptr(), v.len());
        glib_sys::g_variant_ref_sink(r);
        from_glib_full(r)
    }
}

/// Extract a child from a variant.
pub fn variant_get_child_value(v: &glib::Variant, n: usize) -> Option<glib::Variant> {
    let v = v.to_glib_none();
    let l = unsafe { glib_sys::g_variant_n_children(v.0) };
    if n >= l {
        None
    } else {
        unsafe { from_glib_full(glib_sys::g_variant_get_child_value(v.0, n)) }
    }
}

/// Create a new GVariant of type a(ayay).  This is used by OSTree's extended attributes.
pub fn new_variant_a_ayay<T: AsRef<[u8]>>(items: &[(T, T)]) -> glib::Variant {
    unsafe {
        let ty = glib::VariantTy::new("a(ayay)").unwrap();
        let builder = glib_sys::g_variant_builder_new(ty.as_ptr() as *const _);
        for (k, v) in items {
            let k = new_variant_bytearray(k.as_ref());
            let v = new_variant_bytearray(v.as_ref());
            let val = new_variant_tuple(&[k, v]);
            glib_sys::g_variant_builder_add_value(builder, val.to_glib_none().0);
        }
        let v = glib_sys::g_variant_builder_end(builder);
        glib_sys::g_variant_ref_sink(v);
        from_glib_full(v)
    }
}

/// Create a new GVariant of type `as`.  
pub fn new_variant_as(items: &[&str]) -> glib::Variant {
    unsafe {
        let ty = glib::VariantTy::new("as").unwrap();
        let builder = glib_sys::g_variant_builder_new(ty.as_ptr() as *const _);
        for &k in items {
            let k = k.to_variant();
            glib_sys::g_variant_builder_add_value(builder, k.to_glib_none().0);
        }
        let v = glib_sys::g_variant_builder_end(builder);
        glib_sys::g_variant_ref_sink(v);
        from_glib_full(v)
    }
}

/// Extension trait for `glib::VariantDict`.
pub trait VariantDictExt {
    /// Find (and duplicate) a string-valued key in this dictionary.
    fn lookup_str(&self, k: &str) -> Option<String>;
    /// Find a `bool`-valued key in this dictionary.
    fn lookup_bool(&self, k: &str) -> Option<bool>;
}

impl VariantDictExt for glib::VariantDict {
    fn lookup_str(&self, k: &str) -> Option<String> {
        // Unwrap safety: Passing the GVariant type string gives us the right value type
        self.lookup_value(k, Some(glib::VariantTy::new("s").unwrap()))
            .map(|v| v.get_str().unwrap().to_string())
    }

    fn lookup_bool(&self, k: &str) -> Option<bool> {
        // Unwrap safety: Passing the GVariant type string gives us the right value type
        self.lookup_value(k, Some(glib::VariantTy::new("b").unwrap()))
            .map(|v| v.get().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUF: &[u8] = &[1u8; 4];

    #[test]
    fn test_variant_from_bytes() {
        let bytes = glib::Bytes::from_static(BUF);
        let v = variant_new_from_bytes("u", bytes, false);
        let val: u32 = v.get().unwrap();
        assert_eq!(val, 16843009);
    }

    #[test]
    fn test_variantdict() {
        let d = glib::VariantDict::new(None);
        d.insert("foo", &"bar");
        assert_eq!(d.lookup_str("foo"), Some("bar".to_string()));
    }

    #[test]
    fn test_variant_as() {
        let _ = new_variant_as(&[]);
        let v = new_variant_as(&["foo", "bar"]);
        assert_eq!(
            variant_get_child_value(&v, 0).unwrap().get_str().unwrap(),
            "foo"
        );
        assert_eq!(
            variant_get_child_value(&v, 1).unwrap().get_str().unwrap(),
            "bar"
        );
        assert!(variant_get_child_value(&v, 2).is_none());
    }
}
