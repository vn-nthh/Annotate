//! A dispatch process source releases the worker even if its parent crashes
//! during inference. It needs no polling loop or resident Rust thread.
use std::ffi::c_void;
#[link(name = "System")]
extern "C" {
    static _dispatch_source_type_proc: c_void;
    fn dispatch_get_global_queue(priority: isize, flags: usize) -> *mut c_void;
    fn dispatch_source_create(
        kind: *const c_void,
        handle: usize,
        mask: usize,
        queue: *mut c_void,
    ) -> *mut c_void;
    fn dispatch_source_set_event_handler_f(
        source: *mut c_void,
        handler: extern "C" fn(*mut c_void),
    );
    fn dispatch_resume(source: *mut c_void);
    fn getppid() -> i32;
    fn _exit(status: i32) -> !;
}
extern "C" fn parent_exited(_: *mut c_void) {
    unsafe {
        _exit(0);
    }
}
pub fn watch_parent() {
    unsafe {
        let parent = getppid();
        if parent <= 1 {
            _exit(0);
        }
        let source = dispatch_source_create(
            std::ptr::addr_of!(_dispatch_source_type_proc),
            parent as usize,
            0x80000000,
            dispatch_get_global_queue(0, 0),
        );
        if source.is_null() {
            eprintln!("Could not watch the parent process");
            _exit(1);
        }
        // The source owns this registration until the worker process exits.
        dispatch_source_set_event_handler_f(source, parent_exited);
        dispatch_resume(source);
        if getppid() != parent {
            _exit(0);
        }
    }
    if std::env::var_os("ANNOTATE_UI_TEST").is_some()
        && std::env::args().any(|arg| arg == "--check-parent-lifetime")
    {
        use std::io::Write;
        println!("ready");
        std::io::stdout().flush().unwrap();
        // Exercise parent termination while the normal command loop is busy.
        std::thread::sleep(std::time::Duration::from_secs(30));
        std::process::exit(1);
    }
}
