//! Embedded CPython running yt-dlp.
//!
//! The interpreter is started once per process with an isolated configuration
//! (explicit home, no `PYTHON*` environment variables, no signal handlers), then
//! jobs run on dedicated worker threads so async callers never block on Python.

use std::ffi::{CString, c_char};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

use pyo3::ffi;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyCFunction, PyDict, PyModule, PyTuple};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const BRIDGE_PY: &str = include_str!("../python/ytmdl_bridge.py");
const WORKER_STACK: usize = 8 * 1024 * 1024;

static STARTED: AtomicBool = AtomicBool::new(false);

/// Where the runtime finds its pieces. All directories must be writable except
/// `python_home` and `ytdlp_seed`.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// CPython home: the directory that contains `lib/python3.14`.
    pub python_home: PathBuf,
    /// yt-dlp zipimport build shipped with the app.
    pub ytdlp_seed: PathBuf,
    /// Updated yt-dlp builds and the `active` pointer file.
    pub ytdlp_dir: PathBuf,
    /// QuickJS-NG executable. yt-dlp requires the file name `qjs`.
    pub qjs: PathBuf,
    /// yt-dlp cache and the Android CA bundle.
    pub cache_dir: PathBuf,
    /// Temporary files; yt-dlp writes JS challenge scripts here.
    pub tmp_dir: PathBuf,
    /// Concurrent downloads.
    pub download_workers: usize,
}

/// Versions reported by the running interpreter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub python: String,
    pub platform: String,
    pub openssl: String,
    pub yt_dlp: Option<String>,
    pub yt_dlp_path: Option<String>,
}

/// One progress report from yt-dlp.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Progress {
    /// `downloading`, `finished` or `error`.
    pub status: String,
    pub downloaded_bytes: Option<u64>,
    pub total_bytes: Option<u64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds remaining.
    pub eta: Option<f64>,
    pub filename: Option<String>,
}

impl Progress {
    pub fn fraction(&self) -> Option<f64> {
        match (self.downloaded_bytes, self.total_bytes) {
            (Some(d), Some(t)) if t > 0 => Some(d as f64 / t as f64),
            _ => None,
        }
    }
}

/// Cooperative cancellation for a running download.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Stable,
    Nightly,
}

impl Channel {
    fn as_str(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Nightly => "nightly",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateOutcome {
    pub updated: bool,
    pub version: String,
    pub path: Option<PathBuf>,
}

type Job = Box<dyn for<'py> FnOnce(Python<'py>, &Bound<'py, PyModule>) + Send>;

struct Lane {
    tx: mpsc::Sender<Job>,
}

impl Lane {
    fn spawn(name: &str, threads: usize, bridge: Arc<Py<PyModule>>) -> Lane {
        let (tx, rx) = mpsc::channel::<Job>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..threads.max(1) {
            let rx = rx.clone();
            let bridge = bridge.clone();
            thread::Builder::new()
                .name(format!("ytmdl-{name}-{i}"))
                .stack_size(WORKER_STACK)
                .spawn(move || {
                    loop {
                        let job = rx.lock().expect("job queue poisoned").recv();
                        let Ok(job) = job else { break };
                        Python::attach(|py| job(py, bridge.bind(py)));
                    }
                })
                .expect("failed to spawn python worker");
        }
        Lane { tx }
    }
}

struct Inner {
    config: RuntimeConfig,
    ytdlp_zip: PathBuf,
    version: VersionInfo,
    meta: Lane,
    downloads: Lane,
}

/// Handle to the process-wide yt-dlp runtime. Cheap to clone.
#[derive(Clone)]
pub struct Runtime(Arc<Inner>);

impl Runtime {
    /// Starts the interpreter and loads yt-dlp. Only one runtime can exist per
    /// process, because CPython cannot be cleanly re-initialized.
    pub fn start(config: RuntimeConfig) -> Result<Runtime> {
        if STARTED.swap(true, Ordering::SeqCst) {
            return Err(Error::AlreadyStarted);
        }
        for dir in [&config.ytdlp_dir, &config.cache_dir, &config.tmp_dir] {
            std::fs::create_dir_all(dir)?;
        }
        init_interpreter(&config.python_home)?;

        let ytdlp_zip = active_ytdlp(&config);
        let (bridge, version) = Python::attach(|py| -> Result<_> {
            let src = CString::new(BRIDGE_PY).expect("bridge source has no NUL");
            let bridge = PyModule::from_code(py, &src, c"ytmdl_bridge.py", c"ytmdl_bridge")
                .map_err(|e| py_error(py, e))?;
            let log = PyCFunction::new_closure(py, None, None, forward_log)
                .map_err(|e| py_error(py, e))?;
            let settings = serde_json::json!({
                "tmp_dir": config.tmp_dir,
                "cache_dir": config.cache_dir,
            });
            let report: String = call(py, &bridge, "configure", (settings.to_string(), log))?;
            tracing::debug!(target: "ytmdl", "python configured: {report}");
            let version: String = call(py, &bridge, "load_ytdlp", (path_str(&ytdlp_zip)?,))?;
            Ok((bridge.unbind(), serde_json::from_str::<VersionInfo>(&version)?))
        })?;
        tracing::info!(
            target: "ytmdl",
            "yt-dlp {} on Python {} ({})",
            version.yt_dlp.as_deref().unwrap_or("?"),
            version.python,
            version.openssl
        );

        let bridge = Arc::new(bridge);
        Ok(Runtime(Arc::new(Inner {
            meta: Lane::spawn("meta", 1, bridge.clone()),
            downloads: Lane::spawn("dl", config.download_workers, bridge),
            config,
            ytdlp_zip,
            version,
        })))
    }

    pub fn config(&self) -> &RuntimeConfig {
        &self.0.config
    }

    pub fn version(&self) -> &VersionInfo {
        &self.0.version
    }

    /// The yt-dlp build this process loaded.
    pub fn ytdlp_zip(&self) -> &Path {
        &self.0.ytdlp_zip
    }

    /// Common yt-dlp arguments: our JS runtime only, our cache directory.
    pub fn base_args(&self) -> Vec<String> {
        let c = &self.0.config;
        vec![
            "--no-js-runtimes".into(),
            "--js-runtimes".into(),
            format!("quickjs:{}", c.qjs.display()),
            "--cache-dir".into(),
            c.cache_dir.join("yt-dlp").display().to_string(),
        ]
    }

    /// Runs `yt_dlp.extract_info(url, download=False)` and returns the sanitized info JSON.
    pub async fn extract_json(&self, args: Vec<String>, url: String) -> Result<String> {
        self.run(&self.0.meta, move |py, bridge| {
            call(py, bridge, "extract", (serde_json::to_string(&args)?, url))
        })
        .await
    }

    /// Downloads `url` and returns the sanitized info JSON of the result.
    pub async fn download_json(
        &self,
        args: Vec<String>,
        url: String,
        progress: impl Fn(Progress) + Send + Sync + 'static,
        cancel: CancelToken,
    ) -> Result<String> {
        self.run(&self.0.downloads, move |py, bridge| {
            let on_progress = PyCFunction::new_closure(
                py,
                None,
                None,
                move |a: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>| -> PyResult<bool> {
                    let json: String = a.get_item(0)?.extract()?;
                    match serde_json::from_str::<Progress>(&json) {
                        Ok(p) => progress(p),
                        Err(e) => tracing::warn!(target: "ytmdl", "bad progress payload: {e}"),
                    }
                    Ok(!cancel.is_cancelled())
                },
            )
            .map_err(|e| py_error(py, e))?;
            call(py, bridge, "download", (serde_json::to_string(&args)?, url, on_progress))
        })
        .await
    }

    /// HTTP GET through yt-dlp's networking (cookies, proxies and TLS settings apply).
    pub async fn fetch(&self, url: String, max_bytes: usize) -> Result<Vec<u8>> {
        self.run(&self.0.meta, move |py, bridge| {
            let data = bridge
                .call_method1("fetch", (url, max_bytes))
                .map_err(|e| py_error(py, e))?;
            let bytes = data.cast::<PyBytes>().map_err(|e| Error::Python(e.to_string()))?;
            Ok(bytes.as_bytes().to_vec())
        })
        .await
    }

    /// Downloads the newest yt-dlp for `channel` if it differs from the loaded one.
    /// It becomes active on the next start.
    pub async fn check_update(&self, channel: Channel) -> Result<UpdateOutcome> {
        let current = self.0.version.yt_dlp.clone().unwrap_or_default();
        let dir = self.0.config.ytdlp_dir.clone();
        let json: String = self
            .run(&self.0.meta, move |py, bridge| {
                call(py, bridge, "check_update", (channel.as_str(), current, path_str(&dir)?))
            })
            .await?;
        let outcome: UpdateOutcome = serde_json::from_str(&json)?;
        if let Some(path) = &outcome.path {
            write_active(&self.0.config.ytdlp_dir, path)?;
        }
        Ok(outcome)
    }

    /// Imports the native stdlib modules yt-dlp needs and reports each result.
    pub async fn check_stdlib(&self) -> Result<serde_json::Value> {
        let report: String = self
            .run(&self.0.meta, |py, bridge| call(py, bridge, "check_stdlib", ()))
            .await?;
        Ok(serde_json::from_str(&report)?)
    }

    /// Solves recorded JS challenges offline through yt-dlp's challenge director.
    pub async fn solve_challenges(
        &self,
        player_url: String,
        player_path: PathBuf,
        n: Vec<String>,
        sig: Vec<String>,
    ) -> Result<serde_json::Value> {
        let args = self.base_args();
        let out: String = self
            .run(&self.0.meta, move |py, bridge| {
                call(
                    py,
                    bridge,
                    "solve_challenges",
                    (
                        serde_json::to_string(&args)?,
                        player_url,
                        path_str(&player_path)?,
                        serde_json::to_string(&n)?,
                        serde_json::to_string(&sig)?,
                    ),
                )
            })
            .await?;
        Ok(serde_json::from_str(&out)?)
    }

    async fn run<T, F>(&self, lane: &Lane, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: for<'py> FnOnce(Python<'py>, &Bound<'py, PyModule>) -> Result<T> + Send + 'static,
    {
        let (tx, rx) = tokio::sync::oneshot::channel();
        lane.tx
            .send(Box::new(move |py, bridge| {
                let _ = tx.send(f(py, bridge));
            }))
            .map_err(|_| Error::Stopped)?;
        rx.await.map_err(|_| Error::Stopped)?
    }
}

/// Starts CPython with an isolated config. Leaves the GIL released.
fn init_interpreter(home: &Path) -> Result<()> {
    let home = CString::new(home.as_os_str().as_encoded_bytes())
        .map_err(|_| Error::Invalid("python home contains NUL".into()))?;
    // SAFETY: called once per process (guarded by STARTED) before any other
    // Python API use; `config` is initialized by PyConfig_InitIsolatedConfig and
    // cleared on every path.
    unsafe {
        if ffi::Py_IsInitialized() != 0 {
            return Err(Error::Init("interpreter already initialized".into()));
        }
        // UTF-8 mode: the host process may never call setlocale(), and yt-dlp
        // restricts filenames when the filesystem encoding can't encode everything.
        let mut pre = std::mem::MaybeUninit::<ffi::PyPreConfig>::uninit();
        ffi::PyPreConfig_InitIsolatedConfig(pre.as_mut_ptr());
        let mut pre = pre.assume_init();
        pre.utf8_mode = 1;
        check_status(ffi::Py_PreInitialize(&pre))?;

        let mut config = std::mem::MaybeUninit::<ffi::PyConfig>::uninit();
        ffi::PyConfig_InitIsolatedConfig(config.as_mut_ptr());
        let mut config = config.assume_init();
        let status = ffi::PyConfig_SetBytesString(
            &mut config,
            &mut config.home,
            home.as_ptr() as *const c_char,
        );
        if let Err(e) = check_status(status) {
            ffi::PyConfig_Clear(&mut config);
            return Err(e);
        }
        let status = ffi::Py_InitializeFromConfig(&config);
        ffi::PyConfig_Clear(&mut config);
        check_status(status)?;
        ffi::PyEval_SaveThread();
    }
    Ok(())
}

fn check_status(status: ffi::PyStatus) -> Result<()> {
    // SAFETY: PyStatus is a plain struct; err_msg/func are static C strings when set.
    unsafe {
        if ffi::PyStatus_Exception(status) == 0 {
            return Ok(());
        }
        let msg = |p: *const c_char| {
            if p.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned()
            }
        };
        Err(Error::Init(format!("{} {}", msg(status.func), msg(status.err_msg))))
    }
}

fn call<'py, T>(
    py: Python<'py>,
    bridge: &Bound<'py, PyModule>,
    name: &str,
    args: impl pyo3::call::PyCallArgs<'py>,
) -> Result<T>
where
    T: FromPyObjectOwned<'py>,
{
    let out = bridge.call_method1(name, args).map_err(|e| py_error(py, e))?;
    out.extract::<T>().map_err(|e| py_error(py, e.into()))
}

fn py_error(py: Python<'_>, err: PyErr) -> Error {
    let kind = err
        .get_type(py)
        .name()
        .map(|n| n.to_string())
        .unwrap_or_default();
    let msg = err.value(py).to_string();
    match kind.as_str() {
        "DownloadCancelled" => Error::Cancelled,
        "DownloadError" | "ExtractorError" | "UnsupportedError" | "GeoRestrictedError" => {
            Error::Extractor(strip_error_prefix(&msg).to_string())
        }
        _ => {
            let tb = err
                .traceback(py)
                .and_then(|tb| tb.format().ok())
                .unwrap_or_default();
            if tb.is_empty() {
                Error::Python(format!("{kind}: {msg}"))
            } else {
                Error::Python(format!("{kind}: {msg}\n{tb}"))
            }
        }
    }
}

fn strip_error_prefix(msg: &str) -> &str {
    msg.strip_prefix("ERROR: ").unwrap_or(msg)
}

fn forward_log(args: &Bound<'_, PyTuple>, _: Option<&Bound<'_, PyDict>>) -> PyResult<()> {
    let level: String = args.get_item(0)?.extract()?;
    let msg: String = args.get_item(1)?.extract()?;
    match level.as_str() {
        "error" => tracing::error!(target: "yt_dlp", "{msg}"),
        "warning" => tracing::warn!(target: "yt_dlp", "{msg}"),
        "info" => tracing::info!(target: "yt_dlp", "{msg}"),
        _ => tracing::debug!(target: "yt_dlp", "{msg}"),
    }
    Ok(())
}

fn path_str(p: &Path) -> Result<String> {
    p.to_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Invalid(format!("non UTF-8 path: {}", p.display())))
}

const ACTIVE_FILE: &str = "active";

/// The yt-dlp build to load: the last successful update, else the seed.
fn active_ytdlp(config: &RuntimeConfig) -> PathBuf {
    let pointer = config.ytdlp_dir.join(ACTIVE_FILE);
    match std::fs::read_to_string(&pointer) {
        Ok(name) => {
            let path = config.ytdlp_dir.join(name.trim());
            if path.is_file() {
                return path;
            }
            tracing::warn!(target: "ytmdl", "{} points to missing {}", pointer.display(), path.display());
            config.ytdlp_seed.clone()
        }
        Err(_) => config.ytdlp_seed.clone(),
    }
}

fn write_active(dir: &Path, zip: &Path) -> Result<()> {
    let name = zip
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Invalid(format!("bad update path {}", zip.display())))?;
    let tmp = dir.join(format!("{ACTIVE_FILE}.part"));
    std::fs::write(&tmp, name)?;
    std::fs::rename(tmp, dir.join(ACTIVE_FILE))?;
    Ok(())
}
