//! The desktop shell.
//!
//! Deliberately thin. Every command here is three lines: take the arguments, call
//! [`rincon_ipc::Api`], map the result. All of the logic — the device registry, the error
//! shape, the redaction, the validation — lives in `rincon-ipc`, which has no Tauri dependency
//! and is therefore covered by the ordinary cross-platform test run.
//!
//! If a command in this file ever grows a decision, that decision belongs in `rincon-ipc`.

// A WebView on Windows must not also open a console window.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use rincon_audio::AudioCapture;
use rincon_core::audio::{AudioFormat, SampleEncoding};
use rincon_core::telemetry::{self, TelemetryConfig};
use rincon_engine::{Dependencies, Engine, EngineConfig};
use rincon_ipc::{Api, DeviceDto, DiagnosticsDto, FirewallDto, IpcResult, SessionDto};
use tauri::{Emitter as _, State};

/// The event the frontend listens on. One per state transition; the frontend never polls.
const SESSION_EVENT: &str = "rincon://session";

#[tauri::command]
async fn scan_devices(api: State<'_, Arc<Api>>) -> IpcResult<Vec<DeviceDto>> {
    api.scan_devices().await
}

#[tauri::command]
async fn connect(api: State<'_, Arc<Api>>, id: String) -> IpcResult<()> {
    api.connect(&id).await
}

#[tauri::command]
async fn disconnect(api: State<'_, Arc<Api>>) -> IpcResult<()> {
    api.disconnect().await
}

#[tauri::command]
async fn set_volume(api: State<'_, Arc<Api>>, level: u8) -> IpcResult<u8> {
    api.set_volume(level).await.map(|volume| volume.get())
}

#[tauri::command]
async fn session_state(api: State<'_, Arc<Api>>) -> IpcResult<SessionDto> {
    Ok(api.session_state())
}

#[tauri::command]
async fn firewall_status(api: State<'_, Arc<Api>>) -> IpcResult<FirewallDto> {
    Ok(api.firewall_status())
}

#[tauri::command]
async fn diagnostics(api: State<'_, Arc<Api>>) -> IpcResult<DiagnosticsDto> {
    Ok(api.diagnostics())
}

fn main() {
    let log_file = dirs_log_path();
    let _ = telemetry::init(&match log_file {
        Some(path) => TelemetryConfig::with_log_file(path),
        None => TelemetryConfig::stderr_only(),
    });

    let api = Arc::new(Api::new(build_engine()));

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Arc::clone(&api))
        .setup(move |app| {
            // Push state changes rather than letting the interface poll. A 4 Hz poll would
            // make the five-step connect stepper look like it skips steps.
            let handle = app.handle().clone();
            let engine = Arc::clone(api.engine());
            let pushed = Arc::clone(&api);
            tauri::async_runtime::spawn(async move {
                let mut states = engine.subscribe();
                while states.recv().await.is_ok() {
                    let _ = handle.emit(SESSION_EVENT, pushed.session_state());
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            scan_devices,
            connect,
            disconnect,
            set_volume,
            session_state,
            firewall_status,
            diagnostics,
        ])
        .run(tauri::generate_context!())
        .expect("the desktop shell failed to start");
}

/// Builds the engine with the real backends.
fn build_engine() -> Arc<Engine> {
    // `Engine::new` already returns an `Arc`; wrapping it again would give the IPC layer a
    // handle to a handle and break the shared-state assumption the event forwarder relies on.
    Engine::new(
        Dependencies {
            discovery: Arc::new(rincon_discovery::SsdpDiscovery::new()),
            control: Arc::new(
                rincon_control::SoapControl::new().expect("the control client must build"),
            ),
            capture: capture_backend(),
        },
        EngineConfig::default(),
    )
}

/// The capture backend, with an env override so the interface can be developed without a
/// speaker or a sound card.
///
/// The `cfg` is not decoration. `rincon_audio::loopback` only exists on platforms that have a
/// loopback backend, so referencing it unconditionally compiles on Windows and fails on Linux
/// — which is exactly the kind of break that only shows up when somebody finally builds the
/// shell on another machine.
fn capture_backend() -> Arc<dyn AudioCapture> {
    if std::env::var("RINCON_SYNTHETIC").is_ok() {
        return Arc::new(synthetic());
    }

    #[cfg(all(feature = "loopback", any(windows, target_os = "macos")))]
    {
        Arc::new(rincon_audio::loopback::LoopbackCapture::new())
    }
    #[cfg(not(all(feature = "loopback", any(windows, target_os = "macos"))))]
    {
        // This build is a compile check or a development shell on a platform with no capture
        // backend. Generated audio is the only honest option, and saying so beats failing at
        // runtime with "no audio device" on a machine that has one.
        tracing::warn!("no loopback backend on this platform; using generated audio");
        Arc::new(synthetic())
    }
}

/// The deterministic backend, for `RINCON_SYNTHETIC` and for platforms with no loopback.
fn synthetic() -> rincon_audio::SyntheticCapture {
    let format =
        AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap_or(AudioFormat::WIRE_DEFAULT);
    rincon_audio::SyntheticCapture::new(format, 1)
}

/// Where the rolling log lives, or `None` if the OS will not tell us.
fn dirs_log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_STATE_HOME"))
        .or_else(|| std::env::var_os("HOME"))?;
    Some(std::path::PathBuf::from(base).join("Rincon").join("rincon.log"))
}
