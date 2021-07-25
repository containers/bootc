//! Extension APIs for working with GVariant.  Not strictly
//! related to ostree, but included here in the interest of
//! avoiding another crate for this.  In the future, some of these
//! may migrate into gtk-rs.

use glib::translate::*;

/// Get the normal form of a GVariant.
pub fn variant_get_normal_form(v: &glib::Variant) -> glib::Variant {
    unsafe { from_glib_full(glib_sys::g_variant_get_normal_form(v.to_glib_none().0)) }
}

/// Create a new GVariant from data.
fn variant_new_from_bytes(ty: &str, bytes: glib::Bytes, trusted: bool) -> glib::Variant {
    unsafe {
        let ty = ty.to_glib_none();
        let ty: *const libc::c_char = ty.0;
        let ty = ty as *const glib_sys::GVariantType;
        let bytes = bytes.to_glib_full();
        let v = glib_sys::g_variant_new_from_bytes(ty, bytes, trusted.into_glib());
        glib_sys::g_variant_ref_sink(v);
        from_glib_full(v)
    }
}

/// Create a normal-form GVariant from raw bytes.
pub(crate) fn variant_normal_from_bytes(ty: &str, bytes: glib::Bytes) -> glib::Variant {
    variant_get_normal_form(&variant_new_from_bytes(ty, bytes, false))
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
            .map(|v| v.str().unwrap().to_string())
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
}
