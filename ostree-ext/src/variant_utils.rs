use glib::translate::*;

#[allow(unsafe_code)]
pub(crate) fn variant_new_from_bytes(ty: &str, bytes: glib::Bytes, trusted: bool) -> glib::Variant {
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

#[allow(unsafe_code)]
pub(crate) fn variant_get_normal_form(v: &glib::Variant) -> glib::Variant {
    unsafe { from_glib_full(glib_sys::g_variant_get_normal_form(v.to_glib_none().0)) }
}

pub(crate) fn variant_normal_from_bytes(ty: &str, bytes: glib::Bytes) -> glib::Variant {
    variant_get_normal_form(&variant_new_from_bytes(ty, bytes, false))
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
