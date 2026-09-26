//! Android: paths and services through JNI, and the runtime unpacked from the APK.
//!
//! The APK carries the CPython stdlib and the seed yt-dlp as one archive,
//! `assets/ytmdl/runtime.zip` (a single file because AGP skips asset directories
//! starting with `_`, like the stdlib's `compression/_common`), and `libpython3.14.so`, its `lib*_python.so` deps and `libqjs.so` (the QuickJS-NG
//! executable) in `lib/arm64-v8a/`. Only `nativeLibraryDir` may be executed from
//! (W^X), so yt-dlp gets `files/bin/qjs`, a symlink to `libqjs.so` there.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use jni::objects::{JObject, JString, JValue};
use jni::{JNIEnv, JavaVM};
use ytmdl_core::RuntimeConfig;

use super::{DOWNLOAD_WORKERS, Prepared};

const RUNTIME_ASSET: &str = "assets/ytmdl/runtime.zip";

pub fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,ytmdl=info".into()),
        )
        .with_ansi(false)
        .without_time()
        .with_writer(logcat::Logcat)
        .init();
    std::panic::set_hook(Box::new(|info| tracing::error!(target: "ytmdl", "panic: {info}")));
}

pub fn prepare() -> Result<Prepared> {
    let paths = with_env(|env, activity| {
        let files = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
        let files = file_path(env, files)?;
        let cache = env.call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])?.l()?;
        let cache = file_path(env, cache)?;
        let info = env
            .call_method(activity, "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", &[])?
            .l()?;
        let native = env.get_field(&info, "nativeLibraryDir", "Ljava/lang/String;")?.l()?;
        let native = string(env, native)?;
        let apk = env.call_method(activity, "getPackageCodePath", "()Ljava/lang/String;", &[])?.l()?;
        let apk = string(env, apk)?;

        let music = env.new_string("Music")?;
        let shared = env
            .call_static_method(
                "android/os/Environment",
                "getExternalStoragePublicDirectory",
                "(Ljava/lang/String;)Ljava/io/File;",
                &[JValue::Object(&music)],
            )?
            .l()?;
        let shared = file_path(env, shared)?;
        let private = env
            .call_method(activity, "getExternalFilesDir", "(Ljava/lang/String;)Ljava/io/File;", &[JValue::Object(&music)])?
            .l()?;
        let private = if private.is_null() { format!("{files}/Music") } else { file_path(env, private)? };
        Ok([files, cache, native, apk, shared, private].map(PathBuf::from))
    })?;
    let [files, cache, native, apk, shared, private] = paths;
    tracing::info!(target: "ytmdl", "files={} native={} apk={}", files.display(), native.display(), apk.display());

    let runtime = files.join("runtime");
    unpack_runtime(&apk, &runtime).context("unpacking the Python runtime from the APK")?;
    let qjs = link_qjs(&native, &files.join("bin")).context("linking qjs")?;

    Ok(Prepared {
        config: RuntimeConfig {
            python_home: runtime.join("python"),
            ytdlp_seed: runtime.join("yt-dlp.zip"),
            ytdlp_dir: files.join("yt-dlp"),
            qjs,
            cache_dir: cache.join("ytmdl"),
            tmp_dir: cache.join("tmp"),
            download_workers: DOWNLOAD_WORKERS,
        },
        output_dir: shared,
        fallback_output_dir: private,
    })
}

/// Unpacks the runtime archive into `dest` once per installed APK.
fn unpack_runtime(apk: &Path, dest: &Path) -> Result<()> {
    let meta = fs::metadata(apk)?;
    let stamp = format!("{} {:?}", meta.len(), meta.modified().ok());
    let stamp_file = dest.join(".apk-stamp");
    if fs::read_to_string(&stamp_file).is_ok_and(|s| s == stamp) {
        return Ok(());
    }
    let started = std::time::Instant::now();
    let staging = dest.with_extension("new");
    let _ = fs::remove_dir_all(&staging);
    fs::create_dir_all(&staging)?;

    // Copy the inner archive out first: zip entries inside the APK are not seekable.
    let archive = staging.join("runtime.zip");
    {
        let mut apk = zip::ZipArchive::new(io::BufReader::new(fs::File::open(apk)?))?;
        let mut entry = apk.by_name(RUNTIME_ASSET).with_context(|| format!("{RUNTIME_ASSET} is not in the APK"))?;
        io::copy(&mut entry, &mut fs::File::create(&archive)?)?;
    }
    let mut runtime = zip::ZipArchive::new(io::BufReader::new(fs::File::open(&archive)?))?;
    let count = runtime.len();
    runtime.extract(&staging)?;
    drop(runtime);
    fs::remove_file(&archive)?;

    fs::write(staging.join(".apk-stamp"), &stamp)?;
    let _ = fs::remove_dir_all(dest);
    fs::rename(&staging, dest)?;
    tracing::info!(target: "ytmdl", "unpacked {count} runtime entries in {:?}", started.elapsed());
    Ok(())
}

/// `bin/qjs -> <nativeLibraryDir>/libqjs.so`; yt-dlp requires the `qjs` name.
fn link_qjs(native_dir: &Path, bin: &Path) -> Result<PathBuf> {
    let target = native_dir.join("libqjs.so");
    if !target.exists() {
        return Err(anyhow!("{} is missing (APK built without it?)", target.display()));
    }
    fs::create_dir_all(bin)?;
    let link = bin.join("qjs");
    if fs::read_link(&link).ok().as_deref() != Some(target.as_path()) {
        let _ = fs::remove_file(&link);
        std::os::unix::fs::symlink(&target, &link)?;
    }
    Ok(link)
}

/// URL passed as `--es ytmdl_autotest <url>` (`autotest` builds), for end-to-end tests.
pub fn autotest_url() -> Option<String> {
    if !cfg!(feature = "autotest") {
        return None;
    }
    with_env(|env, activity| {
        let intent = env.call_method(activity, "getIntent", "()Landroid/content/Intent;", &[])?.l()?;
        if intent.is_null() {
            return Ok(None);
        }
        let key = env.new_string("ytmdl_autotest")?;
        let value = env
            .call_method(&intent, "getStringExtra", "(Ljava/lang/String;)Ljava/lang/String;", &[JValue::Object(&key)])?
            .l()?;
        if value.is_null() { Ok(None) } else { string(env, value).map(Some) }
    })
    .inspect_err(|e| tracing::warn!(target: "ytmdl", "reading autotest extra: {e:#}"))
    .ok()
    .flatten()
}

/// All-files access (API 30+), or the legacy storage permission below that.
pub fn has_storage_access() -> bool {
    with_env(|env, _| {
        if sdk_int(env)? < 30 {
            return Ok(true);
        }
        env.call_static_method("android/os/Environment", "isExternalStorageManager", "()Z", &[])?.z()
    })
    .unwrap_or(false)
}

/// Opens the system page where the user grants all-files access.
pub fn request_storage_access() {
    let result = with_env(|env, activity| {
        let package = env.call_method(activity, "getPackageName", "()Ljava/lang/String;", &[])?.l()?;
        let package = string(env, package)?;
        let action = env.new_string("android.settings.MANAGE_APP_ALL_FILES_ACCESS_PERMISSION")?;
        let uri = env.new_string(format!("package:{package}"))?;
        let uri = env
            .call_static_method("android/net/Uri", "parse", "(Ljava/lang/String;)Landroid/net/Uri;", &[JValue::Object(&uri)])?
            .l()?;
        let intent = env.new_object(
            "android/content/Intent",
            "(Ljava/lang/String;Landroid/net/Uri;)V",
            &[JValue::Object(&action), JValue::Object(&uri)],
        )?;
        env.call_method(activity, "startActivity", "(Landroid/content/Intent;)V", &[JValue::Object(&intent)])?;
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!(target: "ytmdl", "opening storage settings: {e:#}");
    }
}

/// Asks MediaStore to index a finished download so music apps see it.
pub fn media_scan(path: &Path) {
    let result = with_env(|env, activity| {
        let paths = env.new_object_array(1, "java/lang/String", JObject::null())?;
        let path = env.new_string(path.to_string_lossy())?;
        env.set_object_array_element(&paths, 0, path)?;
        env.call_static_method(
            "android/media/MediaScannerConnection",
            "scanFile",
            "(Landroid/content/Context;[Ljava/lang/String;[Ljava/lang/String;Landroid/media/MediaScannerConnection$OnScanCompletedListener;)V",
            &[JValue::Object(activity), JValue::Object(&paths), JValue::Object(&JObject::null()), JValue::Object(&JObject::null())],
        )?;
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!(target: "ytmdl", "media scan of {}: {e:#}", path.display());
    }
}

fn sdk_int(env: &mut JNIEnv) -> jni::errors::Result<i32> {
    env.get_static_field("android/os/Build$VERSION", "SDK_INT", "I")?.i()
}

/// Runs `f` with an attached JNIEnv and the activity, in a local reference frame.
fn with_env<T>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<T>) -> Result<T> {
    let ctx = ndk_context::android_context();
    // SAFETY: tao's glue stores the process JavaVM and a global ref to the activity.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }?;
    let mut env = vm.attach_current_thread()?;
    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let result = env.with_local_frame(32, |env| f(env, &activity));
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    Ok(result?)
}

fn string(env: &mut JNIEnv, obj: JObject) -> jni::errors::Result<String> {
    Ok(env.get_string(&JString::from(obj))?.into())
}

fn file_path(env: &mut JNIEnv, file: JObject) -> jni::errors::Result<String> {
    let path = env.call_method(&file, "getAbsolutePath", "()Ljava/lang/String;", &[])?.l()?;
    string(env, path)
}

/// tracing -> logcat (tag `ytmdl`); stdout/stderr go nowhere in an Android app.
mod logcat {
    use std::ffi::{CString, c_char, c_int};
    use std::io;

    use tracing::{Level, Metadata};
    use tracing_subscriber::fmt::MakeWriter;

    #[link(name = "log")]
    unsafe extern "C" {
        fn __android_log_write(prio: c_int, tag: *const c_char, text: *const c_char) -> c_int;
    }

    pub struct Logcat;

    /// One formatted event; written to logcat on drop.
    pub struct Line {
        prio: c_int,
        buf: Vec<u8>,
    }

    impl io::Write for Line {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(data);
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for Line {
        fn drop(&mut self) {
            let text = String::from_utf8_lossy(&self.buf);
            let text = text.trim_end();
            if text.is_empty() {
                return;
            }
            if let Ok(text) = CString::new(text.replace('\0', "")) {
                // SAFETY: both pointers are valid NUL-terminated strings for the call.
                unsafe { __android_log_write(self.prio, c"ytmdl".as_ptr(), text.as_ptr()) };
            }
        }
    }

    impl<'a> MakeWriter<'a> for Logcat {
        type Writer = Line;

        fn make_writer(&'a self) -> Line {
            Line { prio: 4, buf: Vec::new() }
        }

        fn make_writer_for(&'a self, meta: &Metadata<'_>) -> Line {
            let prio = match *meta.level() {
                Level::ERROR => 6,
                Level::WARN => 5,
                Level::INFO => 4,
                Level::DEBUG => 3,
                Level::TRACE => 2,
            };
            Line { prio, buf: Vec::new() }
        }
    }
}
