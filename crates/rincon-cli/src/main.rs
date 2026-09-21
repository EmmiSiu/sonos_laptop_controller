//! Rincon without a desktop.
//!
//! The CLI exists for three reasons, in ascending order of importance:
//!
//! 1. It is how you debug the audio path without the interface in the way. `rincon stream`
//!    prints a URL you can open in VLC, which separates "capture and serving are correct" from
//!    "the speaker accepts it" — two failures with entirely different causes.
//! 2. It is the [`doctor`](Command::Doctor) command, which answers the question every support
//!    thread starts with: what interfaces are there, which one reached the speaker, and is the
//!    firewall in the way?
//! 3. It is the porting target for webOS. A TV has no Tauri and no WebView; it has a service
//!    binary. Everything the desktop app does is reachable from here, so phase 2 is a new
//!    capture backend and a thin service wrapper rather than a second implementation.

// A command-line tool's entire purpose is writing to stdout.
#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use rincon_audio::{AudioCapture, SyntheticCapture};
use rincon_control::{SoapControl, TransportControl, Volume};
use rincon_core::audio::{AudioFormat, SampleEncoding};
use rincon_core::device::Device;
use rincon_core::telemetry::{self, TelemetryConfig};
use rincon_discovery::{DeviceDiscovery, ScanConfig, SsdpDiscovery};
use rincon_engine::{Dependencies, Engine, EngineConfig, Event, SessionState};

#[derive(Debug, Parser)]
#[command(
    name = "rincon",
    version,
    about = "Play your laptop's audio on a Sonos speaker",
    long_about = None,
)]
struct Cli {
    /// Seconds to spend searching the network.
    #[arg(long, default_value_t = 2, global = true)]
    scan_seconds: u64,

    /// Use generated audio instead of the real capture device.
    ///
    /// Useful on a machine with no output endpoint, and in CI.
    #[arg(long, global = true, env = "RINCON_SYNTHETIC")]
    synthetic: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// List the Sonos players on this network.
    Discover,

    /// Report the network, the audio endpoint, and anything likely to block a session.
    Doctor,

    /// Serve system audio and print the URL, without involving a speaker.
    ///
    /// Open the URL in VLC to check the capture and server halves on their own.
    Stream {
        /// Bind to this local address instead of the first usable interface.
        #[arg(long)]
        interface: Option<IpAddr>,
    },

    /// Stream to a speaker and keep going until interrupted.
    Play {
        /// The room name, as the Sonos app shows it.
        #[arg(long)]
        room: String,

        /// Set the volume before starting.
        #[arg(long)]
        volume: Option<u8>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    // Stderr only: stdout is the command's output, and mixing logs into it would break
    // anything that pipes `rincon discover`.
    let _ = telemetry::init(&TelemetryConfig::stderr_only());

    match &cli.command {
        Command::Discover => discover(&cli).await,
        Command::Doctor => doctor(&cli).await,
        Command::Stream { interface } => stream(&cli, *interface).await,
        Command::Play { room, volume } => play(&cli, room, *volume).await,
    }
}

fn discovery_for(cli: &Cli) -> SsdpDiscovery {
    SsdpDiscovery::with_config(ScanConfig {
        window: Duration::from_secs(cli.scan_seconds.clamp(1, 10)),
        ..ScanConfig::default()
    })
}

async fn discover(cli: &Cli) -> Result<()> {
    let outcome = discovery_for(cli).discover().await.context("discovery could not run")?;

    if outcome.devices.is_empty() && outcome.unresolved.is_empty() {
        println!("No Sonos players answered.\n");
        println!("Things worth checking, in the order they are usually wrong:");
        println!("  1. This computer and the speaker are on the same Wi-Fi network.");
        println!("  2. The network does not have AP/client isolation enabled.");
        println!("  3. Nothing is blocking outbound UDP to 239.255.255.250:1900.");
        println!("\nInterfaces probed: {:?}", outcome.probed_interfaces);
        return Ok(());
    }

    for device in &outcome.devices {
        let via = device.reached_via.map_or_else(|| "?".to_owned(), |address| address.to_string());
        println!(
            "[OK] {:<18} {:<14} {:<15} via {via}",
            device.room.as_str(),
            device.model.as_str(),
            device.address
        );
    }
    for unresolved in &outcome.unresolved {
        println!(
            "[??] {:<33} {:<15} {}",
            "(details unavailable)", unresolved.address, unresolved.reason
        );
    }
    Ok(())
}

async fn doctor(cli: &Cli) -> Result<()> {
    println!("Rincon {} — diagnostics\n", rincon_core::VERSION);

    println!("Network interfaces (best candidate first):");
    for iface in rincon_discovery::scan::probe_interfaces_detailed() {
        let note = match iface.kind {
            rincon_discovery::InterfaceKind::Physical => "physical",
            rincon_discovery::InterfaceKind::Virtual => "virtual switch — deprioritised",
            rincon_discovery::InterfaceKind::Loopback => "loopback",
        };
        println!("  {:<16} {:<12} {}", iface.address, note, iface.name);
    }
    match rincon_discovery::preferred_lan_interface() {
        Some(address) => println!("  -> would bind the stream server to {address}"),
        None => println!("  -> no physical interface; a session would have nowhere to bind"),
    }

    println!("\nAudio endpoint:");
    match capture_backend(cli).start().await {
        Ok(session) => {
            println!("  available, {}", session.format());
            drop(session);
        }
        Err(error) => println!("  unavailable: {error}"),
    }

    println!("\nSpeakers:");
    match discovery_for(cli).discover().await {
        Ok(outcome) if outcome.devices.is_empty() => {
            println!("  none found");
        }
        Ok(outcome) => {
            for device in &outcome.devices {
                println!(
                    "  {} at {} (reached via {:?})",
                    device.room, device.address, device.reached_via
                );
            }
        }
        Err(error) => println!("  scan failed: {error}"),
    }

    println!(
        "\nIf a speaker is listed but playback never starts, the firewall is the usual cause."
    );
    println!("{}\n", rincon_ipc::FIREWALL_EXPLANATION);
    for line in rincon_ipc::FIREWALL_COMMANDS.lines() {
        println!("  {line}");
    }
    println!(
        "\nCheck the network type with:  Get-NetConnectionProfile\n\
         If your home Wi-Fi says Public, that is the thing to fix first."
    );
    Ok(())
}

async fn stream(cli: &Cli, interface: Option<IpAddr>) -> Result<()> {
    // Deliberately not "the first non-loopback address". On a machine with WSL or Hyper-V
    // that is a virtual switch, and binding there produces a URL nothing on the real network
    // can reach -- which looks exactly like a firewall problem.
    let local = match interface {
        Some(address) => address,
        None => rincon_discovery::preferred_lan_interface()
            .map(IpAddr::V4)
            .context("no physical network interface found; pass --interface")?,
    };

    let session = capture_backend(cli).start().await.context("cannot open the audio endpoint")?;
    let format = session.format();
    let (frames, counters, _guard) = session.split();

    let config = rincon_stream::StreamConfig::new(
        std::net::SocketAddr::new(local, 0),
        // Nothing but this machine is allowlisted: `stream` is for checking the audio path,
        // not for serving a speaker. `play` is the command that does that.
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        format.as_wire(),
    )
    // The bound address too. A player on this machine connecting to `192.168.0.153` arrives
    // with that as its source address, not `127.0.0.1`, so allowlisting only loopback refuses
    // the very fetch this command exists to make possible.
    .allowing(local);

    let handle = rincon_stream::bind(config, frames, Arc::clone(&counters))
        .await
        .context("cannot start the stream server")?;

    println!("Serving system audio at:\n\n  {}\n", handle.url.as_str());
    println!("  interface: {local}    format: {}", format.as_wire());
    println!("\nOpen that in VLC to verify capture and serving on their own.");
    println!("Only this machine is allowed to connect. Ctrl-C to stop.\n");

    tokio::signal::ctrl_c().await.context("cannot listen for Ctrl-C")?;

    let snapshot = counters.snapshot();
    println!(
        "\ncaptured {} frames, served {}, dropped {}, underruns {}",
        snapshot.frames_captured,
        snapshot.frames_served,
        snapshot.dropped_total(),
        snapshot.underruns
    );
    handle.shutdown().await;
    Ok(())
}

async fn play(cli: &Cli, room: &str, volume: Option<u8>) -> Result<()> {
    let outcome = discovery_for(cli).discover().await.context("discovery could not run")?;
    let target = find_room(&outcome.devices, room)?;

    let control = SoapControl::new().context("cannot build the control client")?;
    if let Some(level) = volume {
        let level = Volume::new(level).map_err(|error| anyhow::anyhow!("{error}"))?;
        control.set_volume(&target, level).await.context("cannot set the volume")?;
    }

    let engine = Engine::new(
        Dependencies {
            discovery: Arc::new(discovery_for(cli)),
            control: Arc::new(control),
            capture: capture_backend(cli),
        },
        EngineConfig::default(),
    );

    let mut states = engine.subscribe();
    tokio::spawn(async move {
        while let Ok(state) = states.recv().await {
            match &state {
                SessionState::Preparing { step, .. } => {
                    println!("  [{}/6] {}", step.index() + 1, step.label());
                }
                SessionState::Streaming { .. } => {
                    println!("\nPlaying. Expect 1-2 s of delay: that is the speaker's buffer.");
                    println!("Ctrl-C to stop.\n");
                }
                SessionState::Degraded { reason, .. } => println!("  degraded: {reason:?}"),
                SessionState::Failed { reason, .. } => {
                    eprintln!("\n{}", reason.message());
                    eprintln!("{}", reason.remediation());
                }
                _ => {}
            }
        }
    });

    println!("Connecting to {}...", target.room);
    engine.dispatch(Event::Connect(Box::new(target))).await;

    if !matches!(engine.state(), SessionState::Streaming { .. }) {
        bail!("the session did not start");
    }

    tokio::signal::ctrl_c().await.context("cannot listen for Ctrl-C")?;
    println!("\nStopping...");
    engine.dispatch(Event::Disconnect).await;
    engine.dispatch(Event::TeardownComplete).await;
    Ok(())
}

/// Finds a room by name, case-insensitively, with a useful error when it is missing.
fn find_room(devices: &[Device], room: &str) -> Result<Device> {
    if let Some(found) =
        devices.iter().find(|device| device.room.as_str().eq_ignore_ascii_case(room))
    {
        return Ok(found.clone());
    }
    if devices.is_empty() {
        bail!("no speakers found; run `rincon doctor` to see what was probed");
    }
    let known: Vec<&str> = devices.iter().map(|device| device.room.as_str()).collect();
    bail!("no room called `{room}`. Found: {}", known.join(", "))
}

/// The capture backend, honouring `--synthetic`.
fn capture_backend(cli: &Cli) -> Arc<dyn AudioCapture> {
    if cli.synthetic {
        let format =
            AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap_or(AudioFormat::WIRE_DEFAULT);
        return Arc::new(SyntheticCapture::new(format, 1));
    }

    #[cfg(all(feature = "loopback", any(windows, target_os = "macos")))]
    {
        Arc::new(rincon_audio::loopback::LoopbackCapture::new())
    }
    #[cfg(not(all(feature = "loopback", any(windows, target_os = "macos"))))]
    {
        // On a platform with no loopback backend, generated audio is the only honest option.
        // Saying so beats failing with "no audio device" on a machine that has one.
        eprintln!("note: no loopback backend on this platform; using generated audio");
        let format =
            AudioFormat::new(48_000, 2, SampleEncoding::F32Le).unwrap_or(AudioFormat::WIRE_DEFAULT);
        Arc::new(SyntheticCapture::new(format, 1))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        clippy::float_cmp,
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss
    )]

    use clap::CommandFactory as _;
    use rincon_core::device::{DeviceId, DeviceName};

    use super::*;

    fn device(room: &str) -> Device {
        Device {
            // A UDN carries no whitespace, so a room like "Living Room" has to be flattened
            // before it can become one. `DeviceId` enforces that, which is how this surfaced.
            id: DeviceId::new(format!("uuid:RINCON_{}", room.to_uppercase().replace(' ', "_")))
                .unwrap(),
            room: DeviceName::new(room).unwrap(),
            model: DeviceName::new("Sonos One").unwrap(),
            address: "192.168.1.45".parse().unwrap(),
            port: 1400,
            reached_via: None,
        }
    }

    #[test]
    fn the_command_line_is_well_formed() {
        // Catches duplicate short flags and other definition errors that otherwise only show
        // up as a panic the first time a user runs the binary.
        Cli::command().debug_assert();
    }

    #[test]
    fn rooms_are_matched_the_way_a_user_types_them() {
        let devices = vec![device("Kitchen"), device("Living Room")];
        assert_eq!(find_room(&devices, "kitchen").unwrap().room.as_str(), "Kitchen");
        assert_eq!(find_room(&devices, "LIVING ROOM").unwrap().room.as_str(), "Living Room");
    }

    #[test]
    fn a_missing_room_lists_the_ones_that_exist() {
        // "No such room" without saying which rooms do exist is the least helpful possible
        // error for a command whose whole job is finding rooms.
        let devices = vec![device("Kitchen"), device("Study")];
        let error = find_room(&devices, "Bedroom").unwrap_err().to_string();
        assert!(error.contains("Bedroom"));
        assert!(error.contains("Kitchen") && error.contains("Study"), "got: {error}");
    }

    #[test]
    fn no_speakers_at_all_points_at_the_doctor_command() {
        let error = find_room(&[], "Kitchen").unwrap_err().to_string();
        assert!(error.contains("doctor"), "got: {error}");
    }

    #[test]
    fn the_scan_window_cannot_be_set_to_something_absurd() {
        for (requested, expected) in [(0_u64, 1_u64), (2, 2), (999, 10)] {
            let cli = Cli { scan_seconds: requested, synthetic: true, command: Command::Discover };
            let window = Duration::from_secs(cli.scan_seconds.clamp(1, 10));
            assert_eq!(window, Duration::from_secs(expected));
        }
    }
}
