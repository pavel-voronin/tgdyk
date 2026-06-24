#[cfg(not(unix))]
compile_error!("tgdyk currently supports Unix platforms only");

use std::{
    ffi::{CStr, CString, OsString},
    fs::{self, OpenOptions},
    io::{self, Write},
    os::raw::{c_char, c_void},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use directories::BaseDirs;
use libloading::Library;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncWriteExt, BufWriter},
    net::{UnixListener, UnixStream},
    sync::broadcast,
};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

const PRIVATE_DIR_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, global = true, env = "TGDYK_CONFIG")]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(
        about = "Save credentials, authenticate, and store the TDLib session",
        long_about = "Configure Telegram API credentials, authenticate the user, and create the local TDLib session.\n\nRun once before `tgdyk daemon`."
    )]
    Setup,
    #[command(
        about = "Run the local TDLib update daemon",
        long_about = "Start the local TDLib client and publish live updates over the local Unix socket.\n\nRequires `tgdyk setup` first."
    )]
    Daemon,
    #[command(
        about = "Print live daemon updates as NDJSON",
        long_about = "Connect to the running daemon and print live raw TDLib updates as newline-delimited JSON.\n\nThis is live-only; missed updates are not replayed."
    )]
    Stream,
    #[command(
        about = "Check TDLib, credentials, session paths, and daemon connectivity",
        long_about = "Check whether TDLib, credentials, session files, socket, and daemon connectivity are ready.\n\nUse this when setup or streaming does not work."
    )]
    Doctor,
}

#[derive(Default, Deserialize, Serialize)]
struct FileConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    api_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    api_hash: Option<String>,
}

struct Config {
    config_file: PathBuf,
    tdjson_path: Option<PathBuf>,
    api_id: Option<i64>,
    api_hash: Option<String>,
    database_dir: PathBuf,
    files_dir: PathBuf,
    socket_path: PathBuf,
}

struct XdgPaths {
    config_file: PathBuf,
    database_dir: PathBuf,
    files_dir: PathBuf,
    socket_path: PathBuf,
}

struct TdJson {
    _library: Library,
    create: unsafe extern "C" fn() -> *mut c_void,
    send: unsafe extern "C" fn(*mut c_void, *const c_char),
    receive: unsafe extern "C" fn(*mut c_void, f64) -> *const c_char,
    execute: unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char,
    destroy: unsafe extern "C" fn(*mut c_void),
}

struct TdClient {
    api: Arc<TdJson>,
    raw: *mut c_void,
}

// TDLib clients are used from one thread at a time here. Moving ownership to the
// receive thread is fine; sharing calls concurrently would not be.
unsafe impl Send for TdClient {}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let mut config = load_config(&cli)?;

    match cli.command {
        Command::Setup => setup(&mut config),
        Command::Daemon => daemon(config).await,
        Command::Stream => stream(config).await,
        Command::Doctor => doctor(config).await,
    }
}

fn setup(config: &mut Config) -> Result<()> {
    ensure_login_config(config)?;
    let client = create_authorized_client(config, true)?;
    drop(client);
    println!("Telegram session is ready");
    Ok(())
}

async fn daemon(config: Config) -> Result<()> {
    require_existing_session(&config)?;
    let client = create_authorized_client(&config, false)?;
    let listener = bind_socket(&config.socket_path).await?;

    let (updates, _) = broadcast::channel::<String>(1024);
    let running = Arc::new(AtomicBool::new(true));
    let receive_running = Arc::clone(&running);
    let receive_updates = updates.clone();

    let receive_thread =
        thread::spawn(move || receive_loop(client, receive_running, receive_updates));

    info!(socket = %config.socket_path.display(), "daemon ready");

    let result = tokio::select! {
        result = accept_loop(listener, updates) => result,
        result = tokio::signal::ctrl_c() => {
            result.context("failed to listen for shutdown signal").map(|_| {
                info!("shutdown requested");
            })
        }
    };

    running.store(false, Ordering::Relaxed);
    if let Err(error) = receive_thread.join() {
        warn!(?error, "TDLib receive thread panicked");
    }
    if let Err(error) = remove_socket_if_present(&config.socket_path) {
        warn!(%error, socket = %config.socket_path.display(), "failed to remove daemon socket");
    }
    result
}

async fn stream(config: Config) -> Result<()> {
    let mut socket = UnixStream::connect(&config.socket_path)
        .await
        .with_context(|| {
            format!(
                "daemon is not reachable at {}",
                config.socket_path.display()
            )
        })?;
    let mut stdout = tokio::io::stdout();
    tokio::io::copy(&mut socket, &mut stdout)
        .await
        .context("failed to stream daemon output")?;
    Ok(())
}

async fn doctor(config: Config) -> Result<()> {
    let mut ok = true;

    ok &= check(
        "TDLib",
        tdjson_load_status(&config).map(|source| format!("loaded from {source}")),
    );
    ok &= check(
        "Telegram API credentials",
        credentials(&config).map(|_| "configured".to_string()),
    );
    ok &= check(
        "TDLib database",
        require_existing_session(&config).map(|_| config.database_dir.display().to_string()),
    );
    ok &= check(
        "TDLib files directory",
        if config.files_dir.exists() {
            Ok(config.files_dir.display().to_string())
        } else {
            Err(anyhow!("missing {}", config.files_dir.display()))
        },
    );
    ok &= check("daemon socket", daemon_socket_status(&config.socket_path));
    ok &= check(
        "daemon connectivity",
        match tokio::time::timeout(
            Duration::from_millis(500),
            UnixStream::connect(&config.socket_path),
        )
        .await
        {
            Ok(Ok(_)) => Ok("connected".to_string()),
            Ok(Err(error)) => Err(anyhow!(error)),
            Err(_) => Err(anyhow!("connection timed out")),
        },
    );

    if !ok {
        bail!("doctor found problems");
    }

    Ok(())
}

fn receive_loop(client: TdClient, running: Arc<AtomicBool>, updates: broadcast::Sender<String>) {
    while running.load(Ordering::Relaxed) {
        match client.receive(1.0) {
            Ok(Some(update)) => {
                let _ = updates.send(update);
            }
            Ok(None) => {}
            Err(error) => error!(%error, "failed to receive TDLib update"),
        }
    }
}

async fn bind_socket(path: &Path) -> Result<UnixListener> {
    ensure_socket_parent(path)?;

    if path.exists() {
        match UnixStream::connect(path).await {
            Ok(_) => bail!("daemon already appears to be running at {}", path.display()),
            Err(_) if remove_socket_if_present(path)? => {}
            Err(_) => bail!("refusing to remove non-socket file {}", path.display()),
        }
    }

    let listener =
        UnixListener::bind(path).with_context(|| format!("failed to bind {}", path.display()))?;
    set_private_file_permissions(path, PRIVATE_FILE_MODE)?;
    Ok(listener)
}

async fn accept_loop(listener: UnixListener, updates: broadcast::Sender<String>) -> Result<()> {
    loop {
        let (socket, _) = listener.accept().await.context("failed to accept client")?;
        let mut receiver = updates.subscribe();

        tokio::spawn(async move {
            let mut writer = BufWriter::new(socket);

            loop {
                match receiver.recv().await {
                    Ok(update) => {
                        if writer.write_all(update.as_bytes()).await.is_err()
                            || writer.write_all(b"\n").await.is_err()
                            || writer.flush().await.is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        warn!(skipped, "stream client lagged; disconnecting");
                        break;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
}

fn create_authorized_client(config: &Config, interactive: bool) -> Result<TdClient> {
    ensure_private_dir(&config.database_dir)?;
    ensure_private_dir(&config.files_dir)?;

    let api = Arc::new(TdJson::load(config)?);
    api.set_log_verbosity(1)?;

    let client = TdClient::new(api)?;
    drive_authorization(&client, config, interactive)?;
    Ok(client)
}

fn drive_authorization(client: &TdClient, config: &Config, interactive: bool) -> Result<()> {
    loop {
        let Some(raw) = client.receive(10.0)? else {
            continue;
        };
        let update: Value = serde_json::from_str(&raw).context("invalid TDLib JSON")?;

        if type_name(&update) == Some("error") {
            bail!("TDLib error: {}", tdlib_error_message(&update));
        }

        if type_name(&update) != Some("updateAuthorizationState") {
            continue;
        }

        let state = update
            .get("authorization_state")
            .ok_or_else(|| anyhow!("authorization update has no state"))?;
        let state_name =
            type_name(state).ok_or_else(|| anyhow!("authorization state has no type"))?;
        info!(state = state_name, "authorization state");

        match state_name {
            "authorizationStateWaitTdlibParameters" => {
                let (api_id, api_hash) = credentials(config)?;
                client.send(json!({
                    "@type": "setTdlibParameters",
                    "use_test_dc": false,
                    "database_directory": config.database_dir,
                    "files_directory": config.files_dir,
                    "database_encryption_key": "",
                    "use_file_database": true,
                    "use_chat_info_database": true,
                    "use_message_database": true,
                    "use_secret_chats": false,
                    "api_id": api_id,
                    "api_hash": api_hash,
                    "system_language_code": "en",
                    "device_model": "tgdyk",
                    "system_version": std::env::consts::OS,
                    "application_version": env!("CARGO_PKG_VERSION"),
                }))?;
            }
            "authorizationStateWaitEncryptionKey" => {
                client.send(json!({
                    "@type": "checkDatabaseEncryptionKey",
                    "encryption_key": "",
                }))?;
            }
            "authorizationStateReady" => return Ok(()),
            "authorizationStateWaitOtherDeviceConfirmation" => {
                ensure_interactive(
                    interactive,
                    "run `tgdyk setup` to confirm login from another device",
                )?;
                let link = state
                    .get("link")
                    .and_then(Value::as_str)
                    .unwrap_or("<missing confirmation link>");
                println!("Confirm login in Telegram: {link}");
            }
            "authorizationStateWaitPhoneNumber" => {
                ensure_interactive(interactive, "run `tgdyk setup` to enter the phone number")?;
                let phone_number = prompt("Phone number: ")?;
                client.send(json!({
                    "@type": "setAuthenticationPhoneNumber",
                    "phone_number": phone_number,
                    "settings": null,
                }))?;
            }
            "authorizationStateWaitCode" => {
                ensure_interactive(interactive, "run `tgdyk setup` to enter the Telegram code")?;
                let code = prompt_secret("Telegram code: ")?;
                client.send(json!({
                    "@type": "checkAuthenticationCode",
                    "code": code,
                }))?;
            }
            "authorizationStateWaitPassword" => {
                ensure_interactive(interactive, "run `tgdyk setup` to enter the 2FA password")?;
                let hint = state
                    .get("password_hint")
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let label = if hint.is_empty() {
                    "2FA password: ".to_string()
                } else {
                    format!("2FA password ({hint}): ")
                };
                let password = prompt_secret(&label)?;
                client.send(json!({
                    "@type": "checkAuthenticationPassword",
                    "password": password,
                }))?;
            }
            "authorizationStateWaitEmailAddress" => {
                ensure_interactive(interactive, "run `tgdyk setup` to enter the email address")?;
                let email = prompt("Email address: ")?;
                client.send(json!({
                    "@type": "setAuthenticationEmailAddress",
                    "email_address": email,
                }))?;
            }
            "authorizationStateWaitEmailCode" => {
                ensure_interactive(interactive, "run `tgdyk setup` to enter the email code")?;
                let code = prompt_secret("Email code: ")?;
                client.send(json!({
                    "@type": "checkAuthenticationEmailCode",
                    "code": {
                        "@type": "emailAddressAuthenticationCode",
                        "code": code,
                    },
                }))?;
            }
            "authorizationStateWaitRegistration" => {
                ensure_interactive(interactive, "run `tgdyk setup` to finish registration")?;
                let first_name = prompt("First name: ")?;
                let last_name = prompt("Last name (optional): ")?;
                client.send(json!({
                    "@type": "registerUser",
                    "first_name": first_name,
                    "last_name": last_name,
                    "disable_notification": false,
                }))?;
            }
            "authorizationStateLoggingOut"
            | "authorizationStateClosing"
            | "authorizationStateClosed" => {
                bail!("TDLib authorization closed before it became ready")
            }
            other => bail!("unsupported TDLib authorization state: {other}"),
        }
    }
}

fn require_existing_session(config: &Config) -> Result<()> {
    if !config.database_dir.join("td.binlog").exists() {
        bail!(
            "missing TDLib session in {}; run `tgdyk setup` first",
            config.database_dir.display()
        );
    }
    Ok(())
}

fn daemon_socket_status(path: &Path) -> Result<String> {
    if !path.exists() {
        bail!("missing {}", path.display());
    }
    if !is_unix_socket(path)? {
        bail!("not a Unix socket {}", path.display());
    }
    Ok(path.display().to_string())
}

fn credentials(config: &Config) -> Result<(i64, &str)> {
    match (config.api_id, config.api_hash.as_deref()) {
        (Some(api_id), Some(api_hash)) if !api_hash.is_empty() => Ok((api_id, api_hash)),
        _ => bail!("missing Telegram API credentials; run `tgdyk setup` first"),
    }
}

fn ensure_login_config(config: &mut Config) -> Result<()> {
    if config.api_id.is_some()
        && config
            .api_hash
            .as_deref()
            .is_some_and(|api_hash| !api_hash.is_empty())
    {
        return Ok(());
    }

    let mut file = read_file_config(&config.config_file)?;

    if config.api_id.is_none() {
        let api_id = prompt("Telegram API ID: ")?
            .parse()
            .context("invalid Telegram API ID")?;
        config.api_id = Some(api_id);
        file.api_id = Some(api_id);
    }

    if config.api_hash.as_deref().is_none_or(str::is_empty) {
        let api_hash = prompt_secret("Telegram API hash: ")?;
        if api_hash.is_empty() {
            bail!("Telegram API hash is required");
        }
        config.api_hash = Some(api_hash.clone());
        file.api_hash = Some(api_hash);
    }

    save_file_config(&config.config_file, &file)
}

fn ensure_interactive(interactive: bool, message: &str) -> Result<()> {
    if interactive {
        Ok(())
    } else {
        bail!("{message}")
    }
}

fn prompt(label: &str) -> Result<String> {
    print!("{label}");
    io::stdout().flush().context("failed to flush stdout")?;

    let mut value = String::new();
    io::stdin()
        .read_line(&mut value)
        .context("failed to read stdin")?;
    Ok(value.trim().to_string())
}

fn prompt_secret(label: &str) -> Result<String> {
    rpassword::prompt_password(label).context("failed to read secret")
}

impl TdJson {
    fn load(config: &Config) -> Result<Self> {
        let mut errors = Vec::new();

        for source in tdjson_sources(config) {
            match Self::load_one(&source) {
                Ok(api) => {
                    return Ok(api);
                }
                Err(error) => errors.push(format!("{}: {error}", source.to_string_lossy())),
            }
        }

        bail!(
            "failed to load TDLib JSON library; set TDJSON_PATH. Tried: {}",
            errors.join("; ")
        )
    }

    fn load_one(source: &OsString) -> Result<Self> {
        let library = unsafe { Library::new(source) }
            .with_context(|| format!("failed to open {}", source.to_string_lossy()))?;

        let create = unsafe {
            *library
                .get::<unsafe extern "C" fn() -> *mut c_void>(b"td_json_client_create\0")
                .context("missing td_json_client_create")?
        };
        let send = unsafe {
            *library
                .get::<unsafe extern "C" fn(*mut c_void, *const c_char)>(b"td_json_client_send\0")
                .context("missing td_json_client_send")?
        };
        let receive = unsafe {
            *library
                .get::<unsafe extern "C" fn(*mut c_void, f64) -> *const c_char>(
                    b"td_json_client_receive\0",
                )
                .context("missing td_json_client_receive")?
        };
        let execute = unsafe {
            *library
                .get::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char>(
                    b"td_json_client_execute\0",
                )
                .context("missing td_json_client_execute")?
        };
        let destroy = unsafe {
            *library
                .get::<unsafe extern "C" fn(*mut c_void)>(b"td_json_client_destroy\0")
                .context("missing td_json_client_destroy")?
        };

        Ok(Self {
            _library: library,
            create,
            send,
            receive,
            execute,
            destroy,
        })
    }

    fn set_log_verbosity(&self, level: i32) -> Result<()> {
        let request = CString::new(
            json!({
                "@type": "setLogVerbosityLevel",
                "new_verbosity_level": level,
            })
            .to_string(),
        )?;
        let raw = unsafe { (self.execute)(std::ptr::null_mut(), request.as_ptr()) };
        if raw.is_null() {
            return Ok(());
        }

        let response = unsafe { CStr::from_ptr(raw) }.to_string_lossy();
        let value: Value = serde_json::from_str(&response)?;
        if type_name(&value) == Some("error") {
            bail!("failed to set TDLib log verbosity: {value}");
        }
        Ok(())
    }
}

impl TdClient {
    fn new(api: Arc<TdJson>) -> Result<Self> {
        let raw = unsafe { (api.create)() };
        if raw.is_null() {
            bail!("td_json_client_create returned null");
        }
        Ok(Self { api, raw })
    }

    fn send(&self, value: Value) -> Result<()> {
        let request = CString::new(value.to_string())?;
        unsafe { (self.api.send)(self.raw, request.as_ptr()) };
        Ok(())
    }

    fn receive(&self, timeout_seconds: f64) -> Result<Option<String>> {
        let raw = unsafe { (self.api.receive)(self.raw, timeout_seconds) };
        if raw.is_null() {
            return Ok(None);
        }

        Ok(Some(
            unsafe { CStr::from_ptr(raw) }
                .to_string_lossy()
                .into_owned(),
        ))
    }
}

impl Drop for TdClient {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe { (self.api.destroy)(self.raw) };
        }
    }
}

fn tdjson_load_status(config: &Config) -> Result<String> {
    let mut errors = Vec::new();

    for source in tdjson_sources(config) {
        match TdJson::load_one(&source) {
            Ok(_) => return Ok(source.to_string_lossy().into_owned()),
            Err(error) => errors.push(format!("{}: {error}", source.to_string_lossy())),
        }
    }

    bail!("{}", errors.join("; "))
}

fn tdjson_sources(config: &Config) -> Vec<OsString> {
    if let Some(path) = &config.tdjson_path {
        return vec![path.as_os_str().to_owned()];
    }

    let mut sources = Vec::new();

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        sources.push(dir.join(default_tdjson_name()).into_os_string());
    }

    sources.push(OsString::from(default_tdjson_name()));
    sources
}

fn default_tdjson_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libtdjson.dylib"
    } else {
        "libtdjson.so"
    }
}

fn load_config(cli: &Cli) -> Result<Config> {
    let xdg = xdg_paths()?;
    let config_path = cli
        .config
        .clone()
        .unwrap_or_else(|| xdg.config_file.clone());
    let file = read_file_config(&config_path)?;

    Ok(Config {
        config_file: config_path,
        tdjson_path: std::env::var_os("TDJSON_PATH").map(PathBuf::from),
        api_id: file.api_id,
        api_hash: file.api_hash,
        database_dir: xdg.database_dir,
        files_dir: xdg.files_dir,
        socket_path: xdg.socket_path,
    })
}

fn read_file_config(path: &Path) -> Result<FileConfig> {
    if !path.exists() {
        return Ok(FileConfig::default());
    }

    let text = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("failed to parse config {}", path.display()))
}

fn save_file_config(path: &Path, config: &FileConfig) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;

    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }

    let text = toml::to_string_pretty(config).context("failed to serialize config")?;
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .with_context(|| format!("failed to write config {}", path.display()))?;
    file.write_all(text.as_bytes())
        .with_context(|| format!("failed to write config {}", path.display()))?;
    set_private_file_permissions(path, PRIVATE_FILE_MODE)
}

fn ensure_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("failed to create {}", path.display()))?;
    set_private_file_permissions(path, PRIVATE_DIR_MODE)
}

fn ensure_socket_parent(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let Some(parent) = path.parent() else {
        return Ok(());
    };

    if parent.exists() {
        if !parent.is_dir() {
            bail!("socket parent is not a directory: {}", parent.display());
        }
        let mode = fs::metadata(parent)
            .with_context(|| format!("failed to inspect {}", parent.display()))?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o022 != 0 {
            bail!(
                "socket parent has insecure permissions {mode:o}: {}",
                parent.display()
            );
        }
        return Ok(());
    }

    fs::create_dir_all(parent).with_context(|| format!("failed to create {}", parent.display()))?;
    set_private_file_permissions(parent, PRIVATE_DIR_MODE)
}

#[cfg(unix)]
fn is_unix_socket(path: &Path) -> Result<bool> {
    use std::os::unix::fs::FileTypeExt;

    Ok(fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect {}", path.display()))?
        .file_type()
        .is_socket())
}

fn remove_socket_if_present(path: &Path) -> Result<bool> {
    if !path.exists() || !is_unix_socket(path)? {
        return Ok(false);
    }

    fs::remove_file(path).with_context(|| format!("failed to remove socket {}", path.display()))?;
    Ok(true)
}

fn tdlib_error_message(update: &Value) -> String {
    let message = update
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown TDLib error");

    match update.get("code").and_then(Value::as_i64) {
        Some(code) => format!("{code}: {message}"),
        None => message.to_string(),
    }
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .with_context(|| format!("failed to set {mode:o} permissions on {}", path.display()))
}

fn xdg_paths() -> Result<XdgPaths> {
    let base = BaseDirs::new().ok_or_else(|| anyhow!("failed to resolve home directory"))?;

    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.home_dir().join(".config"));
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| base.home_dir().join(".local/share"));
    let socket_path =
        if let Some(runtime_home) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from) {
            runtime_home.join("tgdyk/tgdyk.sock")
        } else {
            let cache_home = std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| base.home_dir().join(".cache"));
            cache_home.join("tgdyk/run/tgdyk.sock")
        };

    Ok(XdgPaths {
        config_file: config_home.join("tgdyk/config.toml"),
        database_dir: data_home.join("tgdyk/tdlib/database"),
        files_dir: data_home.join("tgdyk/tdlib/files"),
        socket_path,
    })
}

fn type_name(value: &Value) -> Option<&str> {
    value
        .get("@type")
        .or_else(|| value.get("_"))
        .and_then(Value::as_str)
}

fn check(label: &str, result: Result<String>) -> bool {
    match result {
        Ok(detail) => {
            println!("ok   {label}: {detail}");
            true
        }
        Err(error) => {
            println!("fail {label}: {error}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn temp_test_dir(name: &str) -> PathBuf {
        let id = NEXT_TEST_DIR.fetch_add(1, AtomicOrdering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "tgdyk-test-{}-{name}-{nanos}-{id}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_tdlib_type_names() {
        assert_eq!(
            type_name(&json!({"@type": "updateAuthorizationState"})),
            Some("updateAuthorizationState")
        );
        assert_eq!(
            type_name(&json!({"_": "updateAuthorizationState"})),
            Some("updateAuthorizationState")
        );
        assert_eq!(type_name(&json!({})), None);
    }

    #[test]
    fn tdlib_error_message_omits_raw_payload() {
        let value = json!({
            "@type": "error",
            "code": 401,
            "message": "Unauthorized",
            "phone_number": "+10000000000"
        });

        assert_eq!(tdlib_error_message(&value), "401: Unauthorized");
    }

    #[test]
    fn saves_api_credentials_to_config() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_test_dir("config");
        let path = dir.join("config.toml");
        let config = FileConfig {
            api_id: Some(12345),
            api_hash: Some("hash".to_string()),
            ..FileConfig::default()
        };

        save_file_config(&path, &config).unwrap();

        let saved = fs::read_to_string(&path).unwrap();
        assert!(saved.contains("api_id = 12345"));
        assert!(saved.contains("api_hash = \"hash\""));
        assert_eq!(
            read_file_config(&path).unwrap().api_hash.as_deref(),
            Some("hash")
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_FILE_MODE);

        fs::remove_file(path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn regular_file_at_socket_path_is_rejected() {
        let dir = temp_test_dir("socket-file");
        let path = dir.join("tgdyk.sock");

        fs::write(&path, b"not a socket").unwrap();

        let error = bind_socket(&path).await.unwrap_err();

        assert!(error.to_string().contains("refusing to remove non-socket"));
        assert!(path.exists());

        let error = daemon_socket_status(&path).unwrap_err();
        assert!(error.to_string().contains("not a Unix socket"));

        fs::remove_file(path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn remove_socket_if_present_keeps_regular_file() {
        let dir = temp_test_dir("remove-socket");
        let path = dir.join("tgdyk.sock");

        fs::write(&path, b"not a socket").unwrap();

        assert!(!remove_socket_if_present(&path).unwrap());
        assert!(path.exists());

        fs::remove_file(path).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn created_socket_parent_is_made_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_test_dir("created-socket-parent");
        let parent = dir.join("run");

        ensure_socket_parent(&parent.join("tgdyk.sock")).unwrap();

        let mode = fs::metadata(&parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, PRIVATE_DIR_MODE);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn existing_socket_parent_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_test_dir("existing-socket-parent");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_socket_parent(&dir.join("tgdyk.sock")).unwrap();

        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writable_socket_parent_is_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let dir = temp_test_dir("writable-socket-parent");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o777)).unwrap();

        let error = ensure_socket_parent(&dir.join("tgdyk.sock")).unwrap_err();

        assert!(
            error
                .to_string()
                .contains("socket parent has insecure permissions")
        );

        fs::remove_dir_all(dir).unwrap();
    }
}
