//! Android: paths and services through JNI, and the runtime unpacked from the APK.
//!
//! The APK carries the CPython stdlib and the seed yt-dlp as one archive,
//! `assets/ytmdl/runtime.zip` (a single file because AGP skips asset directories
//! starting with `_`, like the stdlib's `compression/_common`), and
//! `libpython3.14.so`, its `lib*_python.so` deps and `libqjs.so` (the QuickJS-NG
//! executable) in `lib/arm64-v8a/`. Only `nativeLibraryDir` may be executed from
//! (W^X), so yt-dlp gets `files/bin/qjs`, a symlink to `libqjs.so` there.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow};
use jni::objects::{GlobalRef, JClass, JObject, JString, JValue, JValueOwned};
use jni::{JNIEnv, JavaVM};
use ytmdl_core::RuntimeConfig;

use super::{DOWNLOAD_WORKERS, Dirs};

const RUNTIME_ASSET: &str = "assets/ytmdl/runtime.zip";

/// Media3's logcat tags for playing audio and its session, service and
/// notification (1.9.4; `MCImpl…` is the controller, `MediaNtfMng` the
/// notification): their warnings are kept, like playback and codec errors.
const MEDIA3_TAGS: &[&str] = &[
    "ExoPlayerImpl*",
    "MediaCodec*",
    "DefaultAudioSink",
    "AudioTrackAudioOutput",
    "ATAudioOutputProvider",
    "DecoderAudioRenderer",
    "ProgressiveMediaPeriod",
    "LoadTask",
    "MediaPeriodHolder",
    "MediaSourceList",
    "MediaSession*",
    "MSessionService",
    "MSSLegacyStub",
    "MLSLegacyStub",
    "MCImpl*",
    "MediaController*",
    "MB2ImplLegacy",
    "MBServiceCompat",
    "MediaNtfMng",
    "NotificationProvider",
    "MediaButtonReceiver",
];

/// Logcat (tag `ytmdl`) and files that apkd-log sends to apkd: this code's
/// events, and what the rest of the process logs (all the Kotlin code's lines,
/// Media3's warnings, errors and crashes).
pub fn init_logging() {
    let mut logger = apkd_log::Logger::new("ytmdl")
        .filter("warn,ytmdl=debug,symphonia_core::formats::probe=error")
        .build(apkd_log::Build { commit: option_env!("YTMDL_COMMIT").or(option_env!("APKD_COMMIT")), ..apkd_log::build!() });
    for tag in MEDIA3_TAGS {
        logger = logger.logcat_tag(tag, apkd_log::LogLevel::Warn);
    }
    logger.start();
}

pub fn dirs() -> Result<Dirs> {
    let [data, cache, music, fallback_music] = with_env(|env, activity| {
        let files = env.call_method(activity, "getFilesDir", "()Ljava/io/File;", &[])?.l()?;
        let files = file_path(env, files)?;
        let cache = env.call_method(activity, "getCacheDir", "()Ljava/io/File;", &[])?.l()?;
        let cache = file_path(env, cache)?;
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
        Ok([files, cache, shared, private].map(PathBuf::from))
    })?;
    Ok(Dirs { data, cache, music, fallback_music })
}

/// Unpacks the runtime from the APK (first start after an install) and links qjs.
pub fn prepare(dirs: &Dirs) -> Result<RuntimeConfig> {
    let [native, apk] = with_env(|env, activity| {
        let info = env
            .call_method(activity, "getApplicationInfo", "()Landroid/content/pm/ApplicationInfo;", &[])?
            .l()?;
        let native = env.get_field(&info, "nativeLibraryDir", "Ljava/lang/String;")?.l()?;
        let native = string(env, native)?;
        let apk = env.call_method(activity, "getPackageCodePath", "()Ljava/lang/String;", &[])?.l()?;
        let apk = string(env, apk)?;
        Ok([native, apk].map(PathBuf::from))
    })?;
    let files = &dirs.data;
    tracing::info!(target: "ytmdl", "files={} native={} apk={}", files.display(), native.display(), apk.display());

    let runtime = files.join("runtime");
    unpack_runtime(&apk, &runtime).context("unpacking the Python runtime from the APK")?;
    let qjs = link_qjs(&native, &files.join("bin")).context("linking qjs")?;

    Ok(RuntimeConfig {
        python_home: runtime.join("python"),
        ytdlp_seed: runtime.join("yt-dlp.zip"),
        ytdlp_dir: files.join("yt-dlp"),
        qjs,
        cache_dir: dirs.cache.join("ytmdl"),
        tmp_dir: dirs.cache.join("tmp"),
        download_workers: DOWNLOAD_WORKERS,
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

/// Asks MediaStore to index a finished download so music apps see it, or to
/// forget a deleted one.
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

/// Playback through `YtmdlPlayer` (app/android/YtmdlPlayer.kt), which drives the
/// Media3 service. Every call returns at once; the service reports its state
/// back as JSON through the channel given to [`player::connect`].
pub mod player {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use jni::objects::{GlobalRef, JClass, JString, JValue, JValueOwned};
    use jni::{JNIEnv, NativeMethod};
    use tokio::sync::mpsc::UnboundedSender;

    use super::{app_class, call_static, with_env};

    const CLASS: &str = "dev.nick.ytmdl.YtmdlPlayer";
    static PLAYER: OnceLock<GlobalRef> = OnceLock::new();
    static STATES: OnceLock<UnboundedSender<String>> = OnceLock::new();

    pub fn connect(states: UnboundedSender<String>) {
        let _ = STATES.set(states);
        let result = with_env(|env, activity| {
            let class = app_class(env, activity, CLASS, &PLAYER)?;
            let changed = NativeMethod {
                name: "nativeChanged".into(),
                sig: "(Ljava/lang/String;)V".into(),
                fn_ptr: native_changed as *mut c_void,
            };
            env.register_native_methods(&class, &[changed])?;
            env.call_static_method(&class, "connect", "(Landroid/content/Context;)V", &[JValue::Object(activity)])?;
            Ok(())
        });
        if let Err(e) = result {
            tracing::error!(target: "ytmdl", "connecting to the player: {e:#}");
        }
    }

    /// Called on the Android main thread for every player change.
    extern "system" fn native_changed<'local>(mut env: JNIEnv<'local>, _: JClass<'local>, state: JString<'local>) {
        if let (Ok(state), Some(tx)) = (env.get_string(&state), STATES.get()) {
            let _ = tx.send(state.into());
        }
    }

    fn call(method: &str, sig: &str, args: impl for<'a> FnOnce(&mut JNIEnv<'a>) -> jni::errors::Result<Vec<JValueOwned<'a>>>) {
        call_static(CLASS, &PLAYER, method, sig, |env, _| args(env));
    }

    fn string<'a>(env: &mut JNIEnv<'a>, s: &str) -> jni::errors::Result<JValueOwned<'a>> {
        Ok(JValueOwned::Object(env.new_string(s)?.into()))
    }

    /// `items`: JSON array of `{id, path, title, artist, album, art, gain,
    /// peak}`, ids unique in the queue.
    pub fn set_queue(items: &str, index: usize, position_ms: i64, play: bool) {
        call("setQueue", "(Ljava/lang/String;IJZ)V", |env| {
            Ok(vec![
                string(env, items)?,
                JValueOwned::Int(index as i32),
                JValueOwned::Long(position_ms),
                JValueOwned::Bool(play.into()),
            ])
        });
    }

    /// Adds `items` after the current song (`next`) or at the end.
    pub fn insert(items: &str, next: bool) {
        call("insert", "(Ljava/lang/String;Z)V", |env| Ok(vec![string(env, items)?, JValueOwned::Bool(next.into())]));
    }

    /// Moves the entry at queue position `from` to `to`.
    pub fn move_entry(from: usize, to: usize) {
        call("move", "(II)V", |_| Ok(vec![JValueOwned::Int(from as i32), JValueOwned::Int(to as i32)]));
    }

    /// Pauses at wall-clock `at_ms` (Unix ms), or at the end of the song;
    /// neither clears the timer.
    pub fn set_sleep(at_ms: i64, end_of_song: bool) {
        call("setSleep", "(JZ)V", |_| Ok(vec![JValueOwned::Long(at_ms), JValueOwned::Bool(end_of_song.into())]));
    }

    /// Removes the queue entry with id `key`, or all entries of track `key`.
    pub fn remove(key: &str) {
        call("remove", "(Ljava/lang/String;)V", |env| Ok(vec![string(env, key)?]));
    }

    /// Loops entry `id` from `a_ms` to `b_ms`; `None` clears the loop.
    pub fn set_song_loop(song: Option<(&str, i64, i64)>) {
        let (id, a, b) = song.unwrap_or(("", 0, 0));
        call("setSongLoop", "(Ljava/lang/String;JJ)V", |env| {
            Ok(vec![string(env, id)?, JValueOwned::Long(a), JValueOwned::Long(b)])
        });
    }

    /// Goes back to entry `first` when entry `last` ends; `None` clears.
    pub fn set_queue_loop(range: Option<(&str, &str)>) {
        let (first, last) = range.unwrap_or(("", ""));
        call("setQueueLoop", "(Ljava/lang/String;Ljava/lang/String;)V", |env| {
            Ok(vec![string(env, first)?, string(env, last)?])
        });
    }

    /// Evens out loudness with the songs' ReplayGain (the `gain` of queue items).
    pub fn set_normalize(on: bool) {
        call("setNormalize", "(Z)V", |_| Ok(vec![JValueOwned::Bool(on.into())]));
    }

    pub fn play() {
        call("play", "()V", |_| Ok(Vec::new()));
    }

    pub fn pause() {
        call("pause", "()V", |_| Ok(Vec::new()));
    }

    pub fn next() {
        call("next", "()V", |_| Ok(Vec::new()));
    }

    pub fn previous() {
        call("previous", "()V", |_| Ok(Vec::new()));
    }

    pub fn seek_to(position_ms: i64) {
        call("seekTo", "(J)V", |_| Ok(vec![JValueOwned::Long(position_ms)]));
    }

    pub fn skip_to(index: usize) {
        call("skipTo", "(I)V", |_| Ok(vec![JValueOwned::Int(index as i32)]));
    }

    pub fn set_repeat(mode: i32) {
        call("setRepeat", "(I)V", |_| Ok(vec![JValueOwned::Int(mode)]));
    }
}

/// Text shared to the app from others (`YtmdlShare` in app/android/Share.kt).
pub mod share {
    use std::ffi::c_void;
    use std::sync::OnceLock;

    use jni::objects::{GlobalRef, JClass, JString};
    use jni::{JNIEnv, NativeMethod};
    use tokio::sync::mpsc::UnboundedSender;

    use super::{app_class, with_env};

    const CLASS: &str = "dev.nick.ytmdl.YtmdlShare";
    static SHARE: OnceLock<GlobalRef> = OnceLock::new();
    static TEXTS: OnceLock<UnboundedSender<String>> = OnceLock::new();

    /// Sends each shared text to `texts`, starting with one that started the app.
    pub fn listen(texts: UnboundedSender<String>) {
        let _ = TEXTS.set(texts);
        let result = with_env(|env, activity| {
            let class = app_class(env, activity, CLASS, &SHARE)?;
            let shared = NativeMethod {
                name: "nativeShared".into(),
                sig: "(Ljava/lang/String;)V".into(),
                fn_ptr: native_shared as *mut c_void,
            };
            env.register_native_methods(&class, &[shared])?;
            env.call_static_method(&class, "listen", "()V", &[])?;
            Ok(())
        });
        if let Err(e) = result {
            tracing::error!(target: "ytmdl", "listening for shared links: {e:#}");
        }
    }

    /// Called on the Android main thread.
    extern "system" fn native_shared<'local>(mut env: JNIEnv<'local>, _: JClass<'local>, text: JString<'local>) {
        if let (Ok(text), Some(tx)) = (env.get_string(&text), TEXTS.get()) {
            let _ = tx.send(text.into());
        }
    }
}

/// The downloads foreground service (`YtmdlDownloads` in app/android/Downloads.kt),
/// which keeps the process alive while downloads run in the background.
pub mod downloads {
    use std::sync::OnceLock;

    use jni::objects::{GlobalRef, JValueOwned};

    use super::call_static;

    const CLASS: &str = "dev.nick.ytmdl.YtmdlDownloads";
    static DOWNLOADS: OnceLock<GlobalRef> = OnceLock::new();

    /// Starts the service, or updates its notification.
    pub fn update(title: &str, text: &str) {
        call_static(CLASS, &DOWNLOADS, "update", "(Landroid/content/Context;Ljava/lang/String;Ljava/lang/String;)V", |env, activity| {
            Ok(vec![
                JValueOwned::Object(env.new_local_ref(activity)?),
                JValueOwned::Object(env.new_string(title)?.into()),
                JValueOwned::Object(env.new_string(text)?.into()),
            ])
        });
    }

    pub fn stop() {
        call_static(CLASS, &DOWNLOADS, "stop", "(Landroid/content/Context;)V", |env, activity| {
            Ok(vec![JValueOwned::Object(env.new_local_ref(activity)?)])
        });
    }

    /// Asks for the notification permission (Android 13+) if it isn't granted.
    pub fn ask_notifications() {
        call_static(CLASS, &DOWNLOADS, "askNotifications", "(Landroid/app/Activity;)V", |env, activity| {
            Ok(vec![JValueOwned::Object(env.new_local_ref(activity)?)])
        });
    }
}

/// Loads one of the app's classes, cached in `cache`. App classes need the
/// activity's class loader; JNI's FindClass on a native thread only sees the
/// system classes.
fn app_class<'local>(
    env: &mut JNIEnv<'local>,
    activity: &JObject,
    name: &str,
    cache: &OnceLock<GlobalRef>,
) -> jni::errors::Result<JClass<'local>> {
    if let Some(class) = cache.get() {
        return Ok(JClass::from(env.new_local_ref(class)?));
    }
    let loader = env.call_method(activity, "getClassLoader", "()Ljava/lang/ClassLoader;", &[])?.l()?;
    let name = env.new_string(name)?;
    let class = env
        .call_method(&loader, "loadClass", "(Ljava/lang/String;)Ljava/lang/Class;", &[JValue::Object(&name)])?
        .l()?;
    let _ = cache.set(env.new_global_ref(&class)?);
    Ok(JClass::from(class))
}

/// Calls a static method of an app class, logging failures.
fn call_static(
    class: &str,
    cache: &OnceLock<GlobalRef>,
    method: &str,
    sig: &str,
    args: impl for<'a> FnOnce(&mut JNIEnv<'a>, &JObject) -> jni::errors::Result<Vec<JValueOwned<'a>>>,
) {
    let result = with_env(|env, activity| {
        let class = app_class(env, activity, class, cache)?;
        let args = args(env, activity)?;
        let args: Vec<JValue> = args.iter().map(|a| a.borrow()).collect();
        env.call_static_method(&class, method, sig, &args)?;
        Ok(())
    });
    if let Err(e) = result {
        tracing::warn!(target: "ytmdl", "{class}.{method}: {e:#}");
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
