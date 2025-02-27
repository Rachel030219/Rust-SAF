use std::io::{Read, Write};
use std::ops::Deref;
use std::panic::catch_unwind;
use std::sync::Once;
use std::{ffi::c_void, panic};

use log::{error, info};

use jni::{
    sys::{jint, JNI_VERSION_1_6},
    JavaVM,
};
use ndk_context::{initialize_android_context, release_android_context};
use ndk_saf::AndroidFileOps;
use tracing_logcat::{LogcatMakeWriter, LogcatTag};
use tracing_subscriber::fmt::format::Format;

/// Invalid JNI version constant, signifying JNI_OnLoad failure.
const INVALID_JNI_VERSION: jint = 0;

// Ensure 1-time initialization of JVM
static INIT: Once = Once::new();
static mut JVM: Option<*mut c_void> = None;

#[allow(non_snake_case)]
#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: *mut JavaVM, _: *mut c_void) -> jint {
    let tag = LogcatTag::Fixed(env!("CARGO_PKG_NAME").to_owned());
    let writer = LogcatMakeWriter::new(tag).expect("Failed to initialize logcat writer");

    tracing_subscriber::fmt()
        .event_format(Format::default().with_level(false).without_time())
        .with_writer(writer)
        .with_ansi(false)
        .init();
    panic::set_hook(Box::new(|panic_info| {
        let (filename, line) = panic_info
            .location()
            .map(|loc| (loc.file(), loc.line()))
            .unwrap_or(("<unknown>", 0));

        let cause = panic_info
            .payload()
            .downcast_ref::<String>()
            .map(String::deref);

        let cause = cause.unwrap_or_else(|| {
            panic_info
                .payload()
                .downcast_ref::<&str>()
                .map(|s| *s)
                .unwrap_or("<cause unknown>")
        });

        error!("A panic occurred at {}:{}: {}", filename, line, cause);
    }));
    catch_unwind(|| {
        // Safely init JVM
        INIT.call_once(|| unsafe {
            // Convert *mut JavaVM to *mut c_void and store it
            JVM = Some(vm as *mut c_void);
            info!("JNI_OnLoad called and JVM initialized");
        });
        JNI_VERSION_1_6
    })
    .unwrap_or(INVALID_JNI_VERSION)
}

#[no_mangle]
pub extern "system" fn Java_one_rachelt_rust_1saf_MainActivity_initializeContext(
    _env: *mut jni::JNIEnv,
    _class: jni::objects::JClass,
    context: jni::objects::JObject,
) {
    unsafe {
        // Convert JObject Context to c_void pointer and initialize Context
        if let Some(jvm) = JVM {
            // Converting context to raw pointer
            let context_ptr = context.into_raw() as *mut c_void;

            initialize_android_context(jvm, context_ptr);
        }
    }
    info!("JNI Context initialized");
}

#[no_mangle]
pub extern "system" fn Java_one_rachelt_rust_1saf_MainActivity_releaseContext(
    _env: *mut jni::JNIEnv,
    _class: jni::objects::JClass,
) {
    unsafe {
        release_android_context();
    }
    info!("JNI Context released");
}

pub fn get_jvm() -> Option<*mut c_void> {
    unsafe { JVM }
}

#[no_mangle]
pub extern "system" fn Java_one_rachelt_rust_1saf_MainActivity_listUriFiles(
    _env: *mut jni::JNIEnv,
    _class: jni::objects::JClass,
    uri: jni::objects::JString,
) {
    let vm = get_jvm()
        .map(|jvm| unsafe { JavaVM::from_raw(jvm.cast()) })
        .expect("Couldn't get JVM!")
        .unwrap();
    let mut env = vm.attach_current_thread().expect("Couldn't attach thread!");
    let uri_str: String = env
        .get_string(&uri)
        .expect("Couldn't get java string!")
        .into();
    // Get file info
    let info = ndk_saf::from_tree_url(&uri_str).unwrap();
    let is_dir = info.is_dir;
    info!(
        "Listed files: {:?}, is it DIR? {:?}\nfiles: {:?}",
        info,
        is_dir,
        info.list_files()
    );
    // Create a new directory
    let created_dir = info
        .create_directory("test_dir")
        .expect("Couldn't create dir!");
    info!("Created dir: {:?}", created_dir);
    // Create a new file
    let created = catch_unwind(|| created_dir.create_file("text/plain", "test.mp3"))
        .map_err(|e| {
            error!("{:?}", e);
        })
        .unwrap()
        .unwrap();
    info!("Created file: {:?}", created);
    // Write to our new file
    let mut file = created.open("w").unwrap();
    file.write_all(b"Hello, world!")
        .expect("Couldn't write to file!");
    // And read it back
    let mut file = created.open("r").unwrap();
    let mut content = String::new();
    file.read_to_string(&mut content)
        .expect("Couldn't read file!");
    info!("Content: {:?}", content);
    // List files in the created directory
    let files = created_dir.list_files().expect("Couldn't list files!");
    info!("Files: {:?}", files);
    // Remove the created directory
    let remove_success = created_dir.remove_file().expect("Couldn't remove file!");
    info!("Removed file: {:?}", remove_success);
}
