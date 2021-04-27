//! Extension APIs for working with GVariant.  Not strictly
//! related to ostree, but included here in the interest of
//! avoiding another crate for this.  In the future, some of these
//! may migrate into gtk-rs.

use glib::translate::*;

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
}
