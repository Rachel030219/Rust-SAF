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
                .copied()
                .unwrap_or("<cause unknown>")
        });

        error!("A panic occurred at {}:{}: {}", filename, line, cause);
    }));
    catch_unwind(|| {
        // Safely init JVM and ClassLoader
        INIT.call_once(|| unsafe {
            // Convert *mut JavaVM to *mut c_void and store it
            JVM = Some(vm as *mut c_void);

            // Initialize ClassLoader for proper class finding from non-main threads
            let java_vm = JavaVM::from_raw(vm as *mut jni::sys::JavaVM).unwrap();
            if let Ok(mut env) = java_vm.get_env() {
                if let Err(e) = ndk_saf::initialize_class_loader(vm, &mut env) {
                    error!("JNI_OnLoad: Failed to setup ClassLoader: {:?}", e);
                } else {
                    info!("JNI_OnLoad: JVM and ClassLoader initialized successfully");
                }
            } else {
                error!("JNI_OnLoad: Failed to get JNI environment");
            }
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
    // Add error handling to prevent race conditions during context release
    if let Err(e) = catch_unwind(|| {
        unsafe {
            release_android_context();
        }
        ndk_saf::cleanup_class_loader();
    }) {
        error!("Error during context release: {:?}", e);
    }
    info!("JNI Context released");
}

pub fn get_jvm() -> Option<*mut c_void> {
    unsafe { JVM }
}

#[no_mangle]
pub extern "system" fn Java_one_rachelt_rust_1saf_MainActivity_listUriFiles(
    env: *mut jni::JNIEnv,
    _class: jni::objects::JClass,
    uri: jni::objects::JString,
) {
    // Use the JNIEnv passed from Java instead of creating a new thread attachment
    let env = unsafe { &mut *env };
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

    // Check if the file can be converted to and back from uri
    let created_uri = created.url;
    info!("Getting created file URI: {:?}", created_uri);
    let created_from_uri =
        ndk_saf::from_tree_url(&created_uri).expect("Couldn't convert uri to file info!");
    info!(
        "Constructing from URI again, this time URI: {:?}",
        created_from_uri.url
    );
    // Check if the uri is the same
    info!(
        "Is the URI the same? {}",
        created_from_uri.url == created_uri
    );

    // List files in the created directory
    let files = created_dir.list_files().expect("Couldn't list files!");
    info!("Files: {:?}", files);
    // Remove the created directory
    let remove_success = created_dir.remove_file().expect("Couldn't remove file!");
    info!("Removed file: {:?}", remove_success);
}
