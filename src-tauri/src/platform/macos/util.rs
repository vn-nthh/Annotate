use cocoa::{
    base::{id, nil},
    foundation::NSString,
};
use objc::{msg_send, sel, sel_impl};
use std::{
    ffi::{c_void, CStr},
    panic::{catch_unwind, resume_unwind, AssertUnwindSafe},
};

#[link(name = "System")]
extern "C" {
    fn pthread_main_np() -> i32;
    static _dispatch_main_q: c_void;
    fn dispatch_sync_f(
        queue: *const c_void,
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void),
    );
    fn dispatch_async_f(
        queue: *const c_void,
        context: *mut c_void,
        callback: extern "C" fn(*mut c_void),
    );
}

/// Window mutations run after GPUI releases its current window borrow.
pub fn post_main(call: impl FnOnce() + Send + 'static) {
    extern "C" fn invoke<F: FnOnce()>(data: *mut c_void) {
        let call = unsafe { Box::from_raw(data as *mut F) };
        if catch_unwind(AssertUnwindSafe(|| objc::rc::autoreleasepool(call))).is_err() {
            log::error!("macOS main-thread callback failed");
        }
    }
    fn enqueue<F: FnOnce() + Send + 'static>(call: F) {
        unsafe {
            dispatch_async_f(
                std::ptr::addr_of!(_dispatch_main_q),
                Box::into_raw(Box::new(call)) as _,
                invoke::<F>,
            );
        }
    }
    enqueue(call);
}

/// AppKit stays on GPUI's main thread, including requests from async workers.
pub fn on_main<F, R>(call: F) -> R
where
    F: FnOnce() -> R + Send,
    R: Send,
{
    unsafe {
        if pthread_main_np() != 0 {
            return objc::rc::autoreleasepool(call);
        }
        type Outcome<R> = std::thread::Result<R>;
        extern "C" fn invoke<F: FnOnce() -> R, R>(data: *mut c_void) {
            let (call, result) = unsafe { &mut *(data as *mut (Option<F>, Option<Outcome<R>>)) };
            *result = Some(catch_unwind(AssertUnwindSafe(|| {
                objc::rc::autoreleasepool(call.take().unwrap())
            })));
        }
        let mut context = (Some(call), None::<Outcome<R>>);
        dispatch_sync_f(
            std::ptr::addr_of!(_dispatch_main_q),
            &mut context as *mut _ as _,
            invoke::<F, R>,
        );
        match context.1.unwrap() {
            Ok(value) => value,
            Err(error) => resume_unwind(error),
        }
    }
}

pub unsafe fn nsstr(value: &str) -> id {
    let value = NSString::alloc(nil).init_str(value);
    msg_send![value, autorelease]
}

pub unsafe fn string(value: id) -> String {
    if value == nil {
        return String::new();
    }
    let bytes: *const std::ffi::c_char = msg_send![value, UTF8String];
    if bytes.is_null() {
        String::new()
    } else {
        CStr::from_ptr(bytes).to_string_lossy().into_owned()
    }
}
