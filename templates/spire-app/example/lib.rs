//! Minimal SpireApp core reference (the filled shape): a tiny actor behind the
//! `spire_send_json` FFI, using spire-actor + spire-core.
#![allow(dead_code)]

use std::ffi::{CStr, CString};

// A long-lived child actor (spire-actor pattern): owns one concern, answers
// JSON-shaped messages, spawned once at startup.
struct GreeterActor {
    greeted: usize,
}

impl GreeterActor {
    fn new() -> Self {
        Self { greeted: 0 }
    }
    fn handle_json(&mut self, req: &str) -> String {
        self.greeted += 1;
        let name = req.trim_matches('"');
        format!(
            "{{\"ok\":true,\"result\":{{\"greeting\":\"Hello {name}!\",\"count\":{}}}}}",
            self.greeted
        )
    }
}

// The two FFI symbols the Swift bridge depends on — keep them stable.
#[no_mangle]
pub extern "C" fn spire_send_json(
    request: *const std::os::raw::c_char,
) -> *mut std::os::raw::c_char {
    static mut CORE: Option<GreeterActor> = None;
    let req = unsafe { CStr::from_ptr(request) }.to_string_lossy().to_string();
    let reply = unsafe { CORE.get_or_insert_with(GreeterActor::new) }.handle_json(&req);
    CString::new(reply).map(|c| c.into_raw()).unwrap_or(std::ptr::null_mut())
}

#[no_mangle]
pub extern "C" fn spire_free_string(p: *mut std::os::raw::c_char) {
    if !p.is_null() {
        unsafe { drop(CString::from_raw(p)) };
    }
}
