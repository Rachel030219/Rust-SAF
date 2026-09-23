use std::{
    fs::File,
    os::{fd::FromRawFd, unix::io::RawFd},
};

use crate::jni_utils::{find_class, get_env};
use anyhow::{anyhow, Ok, Result};
use jni::{
    objects::{GlobalRef, JObject, JString, JValueGen},
    JNIEnv,
};
use log::info;

// Android File struct definition
#[derive(Debug, Clone)]
pub struct AndroidFile {
    pub filename: String,     // File name
    pub size: usize,          // File size in bytes, behavior undefined for directories
    pub path: String,         // Path (not valid path, only for display)
    pub url: String,          // Content URI (use THIS to obtain the AndroidFile object again)
    pub is_dir: bool,         // Is the file a directory
    document_file: GlobalRef, // JNI DocumentFile JObject representing the file
}

// Android File system features
pub trait AndroidFileOps {
    fn open(&self, open_mode: &str) -> Result<File>;
    fn list_files(&self) -> Result<Vec<AndroidFile>>;
    fn create_file(&self, mime_type: &str, file_name: &str) -> Result<AndroidFile>;
    fn create_directory(&self, dir_name: &str) -> Result<AndroidFile>;
    fn remove_file(&self) -> Result<bool>;
}

fn get_global_context(env: &mut JNIEnv) -> Result<GlobalRef> {
    let activity_thread = find_class("android/app/ActivityThread")?;
    let current_activity_thread = env
        .call_static_method(
            &activity_thread,
            "currentActivityThread",
            "()Landroid/app/ActivityThread;",
            &[],
        )?
        .l()?;
    let application = env
        .call_method(
            current_activity_thread,
            "getApplication",
            "()Landroid/app/Application;",
            &[],
        )?
        .l()?;
    Ok(env.new_global_ref(application)?)
}

/// Create an AndroidFile object from a content tree URL obtained from Storage Access Framework (SAF).
pub fn from_tree_url(url: &str) -> Result<AndroidFile> {
    info!("Creating AndroidFile object from URL: {}", url);
    // Obtain JNIEnv using improved get_env function
    let mut env_guard = get_env()?;
    let env = &mut *env_guard;
    let context = get_global_context(env)?;

    // Convert Rust string to Java string, and parse it as a URI
    let url_str = env.new_string(url)?;
    let uri = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValueGen::Object(&url_str)],
        )?
        .l()?;

    // Get the parent DocumentFile. Resolve the class through the cached app
    // ClassLoader: a bare FindClass from a Rust-spawned thread uses the boot
    // classloader, which cannot see androidx classes.
    let document_file_class = find_class("androidx/documentfile/provider/DocumentFile")?;
    let parent = env.call_static_method(
        &document_file_class,
        "fromTreeUri",
        "(Landroid/content/Context;Landroid/net/Uri;)Landroidx/documentfile/provider/DocumentFile;",
        &[JValueGen::Object(context.as_obj()), JValueGen::Object(&uri)],
    )?.l()?;

    // Check if parent URI starts with the input URI, in which case we can use the parent directly.
    let parent_uri = env
        .call_method(&parent, "getUri", "()Landroid/net/Uri;", &[])?
        .l()?;

    let parent_uri_str = env
        .call_method(&parent_uri, "toString", "()Ljava/lang/String;", &[])?
        .l()?;

    let input_uri_str = env
        .call_method(&uri, "toString", "()Ljava/lang/String;", &[])?
        .l()?;

    let parent_str: String = env.get_string(&parent_uri_str.into())?.into();
    let input_str: String = env.get_string(&input_uri_str.into())?.into();

    if parent_str.starts_with(&input_str) {
        return Ok(from_document_file(&parent)?);
    }

    // Otherwise, we create a TreeDocumentFile pointing to child file.
    let tree_document_file_class = find_class("androidx/documentfile/provider/TreeDocumentFile")?;
    let document_file = env.new_object(
        tree_document_file_class,
        "(Landroidx/documentfile/provider/DocumentFile;Landroid/content/Context;Landroid/net/Uri;)V",
        &[
            JValueGen::Object(&parent),
            JValueGen::Object(context.as_obj()),
            JValueGen::Object(&uri),
        ],
    )?;

    Ok(from_document_file(&document_file)?)
}

/// Create an AndroidFile object from a DocumentFile Java object.
pub fn from_document_file(document_file: &JObject) -> Result<AndroidFile> {
    info!(
        "Creating AndroidFile object from DocumentFile object: {:?}",
        document_file
    );
    // First, check if document_file is null
    if document_file.is_null() {
        return Err(anyhow!("The provided DocumentFile object is null"));
    }

    // Obtain JNIEnv using improved get_env function
    let mut env_guard = get_env()?;
    let env = &mut *env_guard;

    // Obtain file name
    let filename = env
        .call_method(document_file, "getName", "()Ljava/lang/String;", &[])?
        .l()
        .and_then(|name| {
            env.get_string(&JString::from(name))
                .map(|s| s.to_string_lossy().into_owned())
        })?;

    // Obtain file size
    let size = env.call_method(document_file, "length", "()J", &[])?.j()? as usize;

    // Obtain file path and url
    let uri = env
        .call_method(document_file, "getUri", "()Landroid/net/Uri;", &[])?
        .l()?;
    let path_object = env
        .call_method(&uri, "getPath", "()Ljava/lang/String;", &[])?
        .l()?;
    let path = env
        .get_string(&JString::from(path_object))
        .map(|s| s.to_string_lossy().into_owned())?;
    let url = env
        .call_method(&uri, "toString", "()Ljava/lang/String;", &[])?
        .l()
        .and_then(|url| {
            env.get_string(&JString::from(url))
                .map(|s| s.to_string_lossy().into_owned())
        })?;

    // Check if the URL points to a directory
    let is_dir = env
        .call_method(document_file, "isDirectory", "()Z", &[])?
        .z()
        .unwrap_or(false);

    // Create GlobalRef from DocumentFile object
    let document_file_ref = env.new_global_ref(document_file)?;

    // Construct AndroidFile struct
    Ok(AndroidFile {
        filename,
        size,
        path,
        url,
        is_dir,
        document_file: document_file_ref,
    })
}

pub fn open_content_url(url: &str, open_mode: &str) -> Result<File> {
    info!("Opening file url: {}, with mode: {}", url, open_mode);

    // Obtain JNIEnv and Context using improved get_env function
    let mut env_guard = get_env()?;
    let env = &mut *env_guard;
    let context = get_global_context(env)?;

    // Get ContentResolver object from Context
    let content_resolver = env
        .call_method(
            context,
            "getContentResolver",
            "()Landroid/content/ContentResolver;",
            &[],
        )?
        .l()?;

    // Convert URI string to Java Uri object, open mode to Java string
    let url_str = env.new_string(url)?;
    let uri = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValueGen::Object(&url_str)],
        )?
        .l()?;
    let mode_str = env.new_string(open_mode)?;

    // Open the file descriptor and detach it
    let parcel_fd = env
        .call_method(
            content_resolver,
            "openFileDescriptor",
            "(Landroid/net/Uri;Ljava/lang/String;)Landroid/os/ParcelFileDescriptor;",
            &[JValueGen::Object(&uri), JValueGen::Object(&mode_str)],
        )?
        .l()?;
    let fd = env.call_method(parcel_fd, "detachFd", "()I", &[])?.i()? as RawFd;

    // Validate file descriptor before creating File object
    if fd < 0 {
        return Err(anyhow!("Invalid file descriptor: {}", fd));
    }

    // Create a new file from the validated file descriptor
    let file = unsafe { File::from_raw_fd(fd) };
    Ok(file)
}

impl AndroidFileOps for AndroidFile {
    /// Open the file represented by the AndroidFile object with the specified open mode.
    /// The "open_mode" str corresponds to that in Android ContentResolver.openFileDescriptor method,
    /// which can be "r", "w", "wt", "wa", "rw" or "rwt". Please note that there is <b>no</b> standard
    /// behavior for each mode, and the behavior may vary among different Android versions and file
    /// providers. For example, "w" may truncate the file or not, so it is recommended to specify the
    /// mode explicitly.
    /// Furthermore, "rw" mode requires an on-disk file that supports seeking, while "r" mode and "w"
    /// mode can be used to read or write to a pipe or socket, respectively.
    fn open(&self, open_mode: &str) -> Result<File> {
        // No, you would not want to use this method to open a directory
        if self.is_dir {
            return Err(anyhow!("The provided URL points to a directory"));
        }

        open_content_url(&self.url, open_mode)
    }

    /// List files in the directory represented by the AndroidFile object. If the object does not
    /// represent a tree directory, an error will be returned.
    fn list_files(&self) -> Result<Vec<AndroidFile>> {
        // Check if the DocumentFile object represents a directory
        if !self.is_dir {
            return Err(anyhow!("The provided URL does not point to a directory"));
        }
        info!("Listing files in directory: {}", self.url);

        // Obtain JNIEnv using improved get_env function
        let mut env_guard = get_env()?;
        let env = &mut *env_guard;
        let context = get_global_context(env)?;

        // Get ContentResolver
        let content_resolver = env
            .call_method(
                context.as_obj(),
                "getContentResolver",
                "()Landroid/content/ContentResolver;",
                &[],
            )?
            .l()?;

        // Parse parent URI from self.url
        let parent_uri_str = env.new_string(&self.url)?;
        let parent_uri = env
            .call_static_method(
                "android/net/Uri",
                "parse",
                "(Ljava/lang/String;)Landroid/net/Uri;",
                &[JValueGen::Object(&parent_uri_str)],
            )?
            .l()?;

        let documents_contract_class = "android/provider/DocumentsContract";
        // Get document ID of parent URI
        let parent_document_id = env
            .call_static_method(
                documents_contract_class,
                "getDocumentId",
                "(Landroid/net/Uri;)Ljava/lang/String;",
                &[JValueGen::Object(&parent_uri)],
            )?
            .l()?;

        // Build children URI
        let children_uri = env
            .call_static_method(
                documents_contract_class,
                "buildChildDocumentsUriUsingTree",
                "(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;",
                &[
                    JValueGen::Object(&parent_uri),
                    JValueGen::Object(&parent_document_id),
                ],
            )?
            .l()?;

        // Define projection
        let document_class = "android/provider/DocumentsContract$Document";
        let column_document_id = env
            .get_static_field(document_class, "COLUMN_DOCUMENT_ID", "Ljava/lang/String;")?
            .l()?;
        let column_display_name = env
            .get_static_field(document_class, "COLUMN_DISPLAY_NAME", "Ljava/lang/String;")?
            .l()?;
        let column_size = env
            .get_static_field(document_class, "COLUMN_SIZE", "Ljava/lang/String;")?
            .l()?;
        let column_mime_type = env
            .get_static_field(document_class, "COLUMN_MIME_TYPE", "Ljava/lang/String;")?
            .l()?;

        let projection = env.new_object_array(4, "java/lang/String", JObject::null())?;
        env.set_object_array_element(&projection, 0, column_document_id)?;
        env.set_object_array_element(&projection, 1, column_display_name)?;
        env.set_object_array_element(&projection, 2, column_size)?;
        env.set_object_array_element(&projection, 3, column_mime_type)?;

        // Query
        let cursor = env
            .call_method(
                &content_resolver,
                "query",
                "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
                &[
                    JValueGen::Object(&children_uri),
                    JValueGen::Object(&projection),
                    JValueGen::Object(&JObject::null()),
                    JValueGen::Object(&JObject::null()),
                    JValueGen::Object(&JObject::null()),
                ],
            )?
            .l()?;

        // Get MIME type for directory to compare against
        let mime_type_dir = env
            .get_static_field(document_class, "MIME_TYPE_DIR", "Ljava/lang/String;")?
            .l()?;

        // Resolve via the cached app ClassLoader: a bare FindClass from a
        // Rust-spawned thread hits the boot classloader and misses androidx.
        let document_file_class = find_class("androidx/documentfile/provider/DocumentFile")?;

        let mut files = Vec::new();
        // Check if cursor is not null
        if !cursor.is_null() {
            // Iterate through the cursor
            while env.call_method(&cursor, "moveToNext", "()Z", &[])?.z()? {
                // Get column values
                let doc_id_jstr: JString = env
                    .call_method(
                        &cursor,
                        "getString",
                        "(I)Ljava/lang/String;",
                        &[JValueGen::Int(0)],
                    )?
                    .l()?
                    .into();
                let _doc_id = env.get_string(&doc_id_jstr)?;

                let filename_jstr: JString = env
                    .call_method(
                        &cursor,
                        "getString",
                        "(I)Ljava/lang/String;",
                        &[JValueGen::Int(1)],
                    )?
                    .l()?
                    .into();
                let filename = env
                    .get_string(&filename_jstr)?
                    .to_string_lossy()
                    .into_owned();

                let size = env
                    .call_method(&cursor, "getLong", "(I)J", &[JValueGen::Int(2)])?
                    .j()? as usize;

                let mime_type_jstr: JString = env
                    .call_method(
                        &cursor,
                        "getString",
                        "(I)Ljava/lang/String;",
                        &[JValueGen::Int(3)],
                    )?
                    .l()?
                    .into();

                // Build child URI
                let child_uri = env
                    .call_static_method(
                        documents_contract_class,
                        "buildDocumentUriUsingTree",
                        "(Landroid/net/Uri;Ljava/lang/String;)Landroid/net/Uri;",
                        &[
                            JValueGen::Object(&parent_uri),
                            JValueGen::Object(&doc_id_jstr),
                        ],
                    )?
                    .l()?;

                // Get path and url from child URI
                let path_object = env
                    .call_method(&child_uri, "getPath", "()Ljava/lang/String;", &[])?
                    .l()?;
                let path = env
                    .get_string(&JString::from(path_object))?
                    .to_string_lossy()
                    .into_owned();
                let url = env
                    .call_method(&child_uri, "toString", "()Ljava/lang/String;", &[])?
                    .l()
                    .and_then(|url| {
                        env.get_string(&JString::from(url))
                            .map(|s| s.to_string_lossy().into_owned())
                    })?;

                // Check if it's a directory
                let is_dir = env
                    .call_method(
                        &mime_type_jstr,
                        "equals",
                        "(Ljava/lang/Object;)Z",
                        &[JValueGen::Object(&mime_type_dir)],
                    )?
                    .z()?;

                // Create DocumentFile object
                let document_file = env
                    .call_static_method(
                        &document_file_class,
                        "fromSingleUri",
                        "(Landroid/content/Context;Landroid/net/Uri;)Landroidx/documentfile/provider/DocumentFile;",
                        &[JValueGen::Object(context.as_obj()), JValueGen::Object(&child_uri)],
                    )?
                    .l()?;

                if !document_file.is_null() {
                    let document_file_ref = env.new_global_ref(&document_file)?;

                    files.push(AndroidFile {
                        filename,
                        size,
                        path,
                        url,
                        is_dir,
                        document_file: document_file_ref,
                    });
                }
            }
            // Close the cursor
            env.call_method(&cursor, "close", "()V", &[])?.v()?;
        }

        // Sort files by name
        files.sort_by(|a, b| a.filename.cmp(&b.filename));

        Ok(files)
    }

    /// Create a new file in the directory represented by the AndroidFile object.
    /// If self does not represent a directory, an error will be returned. <br />
    /// PARAMS: MIME type and file name.
    /// The MIME type should be a valid MIME type string, and the file name should not contain any
    /// path separator. When MIME type and extension in file name mismatch, a correct extension will
    /// be appended (thus it is recommended not to include extension).
    /// When names collide, a number will be appended. <br />
    /// RETURNS: A new AndroidFile object representing the newly created file. <br />
    fn create_file(&self, mime_type: &str, file_name: &str) -> Result<AndroidFile> {
        // Check if the DocumentFile object represents a directory
        if !self.is_dir {
            return Err(anyhow!("The provided URL does not point to a directory"));
        }
        info!(
            "Creating file named {} with MIME type {} in directory: {}",
            file_name, mime_type, self.url
        );

        // Obtain JNIEnv using improved get_env function
        let mut env_guard = get_env()?;
        let env = &mut *env_guard;

        // Convert MIME type and file name to Java strings
        let mime_type_str = env.new_string(mime_type)?;
        let file_name_str = env.new_string(file_name)?;

        // Create a new file in the directory
        let new_file = env.call_method(
            &self.document_file,
            "createFile",
            "(Ljava/lang/String;Ljava/lang/String;)Landroidx/documentfile/provider/DocumentFile;",
            &[JValueGen::Object(&mime_type_str), JValueGen::Object(&file_name_str)],
        )?.l()?;

        Ok(from_document_file(&new_file)?)
    }

    /// Create a new directory in the directory represented by the AndroidFile object.
    /// If self does not represent a directory, an error will be returned. <br />
    /// PARAMS: Directory name. When names collide, the file name will be appended with a number. <br />
    /// RETURNS: A new AndroidFile object representing the newly created directory. <br />
    fn create_directory(&self, dir_name: &str) -> Result<AndroidFile> {
        // Check if the DocumentFile object represents a directory
        if !self.is_dir {
            return Err(anyhow!("The provided URL does not point to a directory"));
        }
        info!(
            "Creating directory named {} in directory: {}",
            dir_name, self.url
        );

        // Obtain JNIEnv using improved get_env function
        let mut env_guard = get_env()?;
        let env = &mut *env_guard;

        // Convert directory name to Java string
        let file_name_str = env.new_string(dir_name)?;

        // Create a new file in the directory
        let new_dir = env
            .call_method(
                &self.document_file,
                "createDirectory",
                "(Ljava/lang/String;)Landroidx/documentfile/provider/DocumentFile;",
                &[JValueGen::Object(&file_name_str)],
            )?
            .l()?;

        Ok(from_document_file(&new_dir)?)
    }

    /// Remove the file or directory represented by the AndroidFile object. If the object represents
    /// a directory, the directory will be removed recursively. The method will return true if the
    /// file or directory is removed successfully, or false if the file or directory does not exist.
    fn remove_file(&self) -> Result<bool> {
        // Obtain JNIEnv using improved get_env function
        let mut env_guard = get_env()?;
        let env = &mut *env_guard;

        // Delete the file or directory
        let result = env
            .call_method(self.document_file.as_obj(), "delete", "()Z", &[])?
            .z()?;

        Ok(result)
    }
}
