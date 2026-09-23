use std::sync::{Once, RwLock};

use jni::{
    objects::{GlobalRef, JClass, JMethodID},
    AttachGuard, JNIEnv, JavaVM,
};
use log::{error, info};

// Thread-safe global state for ClassLoader caching and JavaVM storage
static INIT: Once = Once::new();
static CLASS_LOADER: RwLock<Option<GlobalRef>> = RwLock::new(None);
static FIND_CLASS_METHOD: RwLock<Option<JMethodID>> = RwLock::new(None);
static JVM: RwLock<Option<&'static JavaVM>> = RwLock::new(None);

/// Initialize the ClassLoader cache with the correct ClassLoader
pub fn initialize_class_loader(
    vm: *mut JavaVM,
    env: &mut JNIEnv,
) -> Result<(), jni::errors::Error> {
    INIT.call_once(|| {
        // Store the JavaVM for later use
        if let Ok(mut jvm_lock) = JVM.write() {
            match unsafe { JavaVM::from_raw(vm as *mut jni::sys::JavaVM) } {
                Ok(java_vm) => {
                    // Leak the JavaVM to get a 'static reference
                    let static_vm = Box::leak(Box::new(java_vm));
                    *jvm_lock = Some(static_vm);
                    info!("JavaVM stored successfully");
                }
                Err(e) => {
                    error!("Failed to create JavaVM from raw pointer: {:?}", e);
                }
            }
        } else {
            error!("Failed to acquire JavaVM write lock");
        }

        // Setup ClassLoader for proper class finding from non-main threads
        match setup_class_loader(env) {
            Ok((class_loader, find_class_method)) => {
                if let (Ok(mut cl_lock), Ok(mut fcm_lock)) =
                    (CLASS_LOADER.write(), FIND_CLASS_METHOD.write())
                {
                    *cl_lock = Some(class_loader);
                    *fcm_lock = Some(find_class_method);
                    info!("ClassLoader initialized successfully");
                } else {
                    error!("Failed to acquire write locks for ClassLoader initialization");
                }
            }
            Err(e) => {
                error!("Failed to setup ClassLoader: {:?}", e);
            }
        }
    });
    Ok(())
}

/// Setup ClassLoader during initialization to cache for later use
fn setup_class_loader(env: &mut JNIEnv) -> Result<(GlobalRef, JMethodID), jni::errors::Error> {
    // Get the Activity Thread object
    let activity_thread_class = env.find_class("android/app/ActivityThread")?;
    let activity_thread = env.call_static_method(
        &activity_thread_class,
        "currentActivityThread",
        "()Landroid/app/ActivityThread;",
        &[],
    )?;

    // Get the Application object
    let application = env.call_method(
        activity_thread.l()?,
        "getApplication",
        "()Landroid/app/Application;",
        &[],
    )?;

    // Get the package name
    let package_name_obj = env.call_method(
        application.l()?,
        "getPackageName",
        "()Ljava/lang/String;",
        &[],
    )?;
    let package_name_jstring = jni::objects::JString::from(package_name_obj.l()?);
    let package_name: String = env.get_string(&package_name_jstring)?.into();

    // Construct the MainActivity class name
    let main_activity_class_name = format!("{}/MainActivity", package_name.replace('.', "/"));

    // Use MainActivity as our reference class to get the correct ClassLoader
    let main_activity_class = env.find_class(&main_activity_class_name)?;
    let class_class = env.get_object_class(&main_activity_class)?;
    let class_loader_class = env.find_class("java/lang/ClassLoader")?;

    // Get the getClassLoader method
    let _get_class_loader_method =
        env.get_method_id(&class_class, "getClassLoader", "()Ljava/lang/ClassLoader;")?;

    // Get the ClassLoader object
    let class_loader_obj = env.call_method(
        &main_activity_class,
        "getClassLoader",
        "()Ljava/lang/ClassLoader;",
        &[],
    )?;

    let class_loader = env.new_global_ref(class_loader_obj.l()?)?;

    // Cache the findClass method ID
    let find_class_method = env.get_method_id(
        &class_loader_class,
        "findClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
    )?;

    Ok((class_loader, find_class_method))
}

/// Improved getEnv function that uses stored JavaVM from JNI_OnLoad
pub fn get_env() -> Result<AttachGuard<'static>, jni::errors::Error> {
    // Use the stored JavaVM from initialize_class_loader
    let jvm_lock = JVM.read().map_err(|_| {
        jni::errors::Error::NullPtr("Failed to acquire JavaVM read lock")
    })?;
    
    let java_vm = jvm_lock.as_ref()
        .ok_or_else(|| {
            jni::errors::Error::NullPtr(
                "JavaVM not initialized via JNI_OnLoad - ensure initialize_class_loader was called",
            )
        })?;
    
    // Attach current thread with error handling
    match java_vm.attach_current_thread() {
        Ok(guard) => Ok(guard),
        Err(e) => {
            error!("Failed to attach current thread: {:?}", e);
            Err(e)
        }
    }
}

/// Generic class finding function that uses the cached ClassLoader
pub fn find_class(class_name: &str) -> Result<JClass<'_>, jni::errors::Error> {
    let mut env_guard = get_env()?;
    let env = &mut *env_guard;

    // A bare FindClass fallback is never acceptable here: on Rust-spawned
    // threads it resolves through the boot classloader, which cannot see app
    // dex classes and turns a configuration bug into a confusing process-level
    // crash. Fail loudly instead so the real cause (ClassLoader cache not
    // initialized) surfaces immediately.
    let class_loader_lock = CLASS_LOADER.read().map_err(|_| {
        jni::errors::Error::NullPtr("ClassLoader lock poisoned")
    })?;
    let class_loader = class_loader_lock.as_ref().ok_or_else(|| {
        jni::errors::Error::NullPtr(
            "App ClassLoader not initialized; initialize_class_loader must run first",
        )
    })?;

    let class_name_jstring = env.new_string(class_name)?;
    let result = env.call_method(
        class_loader.as_obj(),
        "findClass",
        "(Ljava/lang/String;)Ljava/lang/Class;",
        &[(&class_name_jstring).into()],
    )?;
    Ok(JClass::from(result.l()?))
}

/// Cleanup function for global references and JavaVM (call when library unloads)
pub fn cleanup_class_loader() {
    // Safely acquire write locks and cleanup
    if let Ok(mut class_loader_lock) = CLASS_LOADER.write() {
        if class_loader_lock.take().is_some() {
            // Global references are automatically cleaned up when dropped
        }
    }

    if let Ok(mut find_class_method_lock) = FIND_CLASS_METHOD.write() {
        *find_class_method_lock = None;
    }

    // Cleanup JavaVM reference (note: leaked memory won't be reclaimed)
    if let Ok(mut jvm_lock) = JVM.write() {
        *jvm_lock = None;
    }

    info!("ClassLoader and JavaVM cleanup completed");
}

/// Check if ClassLoader and JavaVM are properly initialized
pub fn is_class_loader_initialized() -> bool {
    if let (Ok(class_loader_lock), Ok(find_class_method_lock), Ok(jvm_lock)) =
        (CLASS_LOADER.read(), FIND_CLASS_METHOD.read(), JVM.read())
    {
        class_loader_lock.is_some() && find_class_method_lock.is_some() && jvm_lock.is_some()
    } else {
        false
    }
}
