// Included beside the existing engine FFI so the verified host remains safe Rust.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn avian_android_provision(
    operation: u8,
    directory: *const std::ffi::c_char,
    document: *const std::ffi::c_char,
    fingerprint: *const std::ffi::c_char,
    identity: *const u8,
    identity_length: usize,
    transport: *const u8,
    transport_length: usize,
) -> *const std::ffi::c_char {
    if directory.is_null()
        || document.is_null()
        || fingerprint.is_null()
        || identity.is_null()
        || transport.is_null()
        || identity_length == 0
        || identity_length > 65
        || transport_length == 0
        || transport_length > 65
    {
        return std::ptr::null();
    }
    let Ok(directory) = (unsafe { std::ffi::CStr::from_ptr(directory) }).to_str() else {
        return std::ptr::null();
    };
    let Ok(document) = (unsafe { std::ffi::CStr::from_ptr(document) }).to_str() else {
        return std::ptr::null();
    };
    let Ok(fingerprint) = (unsafe { std::ffi::CStr::from_ptr(fingerprint) }).to_str() else {
        return std::ptr::null();
    };
    let identity = unsafe { std::slice::from_raw_parts(identity, identity_length) };
    let transport = unsafe { std::slice::from_raw_parts(transport, transport_length) };
    avian_verified_host::provision(
        operation,
        directory,
        document,
        fingerprint,
        identity,
        transport,
    )
    .ok()
    .and_then(|value| std::ffi::CString::new(value).ok())
    .map_or(std::ptr::null(), |value| value.into_raw())
}
