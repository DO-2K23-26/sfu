use clap::Parser;
use dtls::extension::extension_use_srtp::SrtpProtectionProfile;
use log::info;
use opentelemetry::{/*global,*/ KeyValue};
use opentelemetry_sdk::metrics::{PeriodicReader, SdkMeterProvider};
use opentelemetry_sdk::{runtime, Resource};
use opentelemetry_stdout::MetricsExporterBuilder;
use rouille::Server;
use sfu::{RTCCertificate, ServerConfig};
use std::collections::HashMap;
use std::net::{IpAddr, UdpSocket};
use std::str::FromStr;
use std::sync::mpsc::{self};
use std::sync::Arc;
use std::time::Duration;
use wg::WaitGroup;

mod sync_signal;
mod util;

use sync_signal::*;

#[derive(Default, Debug, Copy, Clone, clap::ValueEnum)]
enum Level {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

impl From<Level> for log::LevelFilter {
    fn from(level: Level) -> Self {
        match level {
            Level::Error => log::LevelFilter::Error,
            Level::Warn => log::LevelFilter::Warn,
            Level::Info => log::LevelFilter::Info,
            Level::Debug => log::LevelFilter::Debug,
            Level::Trace => log::LevelFilter::Trace,
        }
    }
}

#[derive(Parser)]
#[command(name = "SFU Server")]
#[command(author = "Rusty Rain <y@ngr.tc>")]
#[command(version = "0.1.0")]
#[command(about = "An example of SFU Server", long_about = None)]
struct Cli {
    #[arg(long, default_value_t = format!("127.0.0.1"))]
    host: String,
    #[arg(short, long, default_value_t = 8080)]
    signal_port: u16,
    #[arg(long, default_value_t = 3478)]
    media_port_min: u16,
    #[arg(long, default_value_t = 3479)]
    media_port_max: u16,

    #[arg(short, long)]
    force_local_loop: bool,
    #[arg(short, long)]
    debug: bool,
    #[arg(short, long, default_value_t = Level::Info)]
    #[clap(value_enum)]
    level: Level,
}

fn init_meter_provider(
    mut stop_rx: async_broadcast::Receiver<()>,
    wait_group: WaitGroup,
) -> SdkMeterProvider {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        rt.block_on(async move {
            let worker = wait_group.add(1);
            let exporter = MetricsExporterBuilder::default()
                .with_encoder(|writer, data| {
                    Ok(serde_json::to_writer_pretty(writer, &data).unwrap())
                })
                .build();
            let reader = PeriodicReader::builder(exporter, runtime::TokioCurrentThread)
                .with_interval(Duration::from_secs(30))
                .build();
            let meter_provider = SdkMeterProvider::builder()
                .with_reader(reader)
                .with_resource(Resource::new(vec![KeyValue::new("chat", "metrics")]))
                .build();
            let _ = tx.send(meter_provider.clone());

            let _ = stop_rx.recv().await;
            let _ = meter_provider.shutdown();
            worker.done();
            info!("meter provider is gracefully down");
        });
    });

    let meter_provider = rx.recv().unwrap();
    meter_provider
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    // if cli.debug {
    //     env_logger::Builder::new()
    //         .format(|buf, record| {
    //             writeln!(
    //                 buf,
    //                 "{}:{} [{}] {} - {}",
    //                 record.file().unwrap_or("unknown"),
    //                 record.line().unwrap_or(0),
    //                 record.level(),
    //                 chrono::Local::now().format("%H:%M:%S.%6f"),
    //                 record.args()
    //             )
    //         })
    //         .filter(None, cli.level.into())
    //         .init();
    // }

    let certificate = include_bytes!("util/cer.pem").to_vec();
    let private_key = include_bytes!("util/key.pem").to_vec();

    // Figure out some public IP address, since Firefox will not accept 127.0.0.1 for WebRTC traffic.
    let host_addr = if cli.host == "127.0.0.1" && !cli.force_local_loop {
        util::select_host_address()
    } else {
        IpAddr::from_str(&cli.host).map_err(|_| std::io::ErrorKind::InvalidInput)?
    };

    let media_ports: Vec<u16> = (cli.media_port_min..=cli.media_port_max).collect();
    let (stop_tx, stop_rx) = crossbeam_channel::bounded::<()>(1);
    let mut media_port_thread_map = HashMap::new();

    let key_pair = rcgen::KeyPair::generate(&rcgen::PKCS_ECDSA_P256_SHA256)
        .map_err(|_| std::io::ErrorKind::InvalidInput)?; // rcgen::PKCS_ECDSA_P256_SHA256
    let certificates =
        vec![RTCCertificate::from_key_pair(key_pair)
            .map_err(|_| std::io::ErrorKind::InvalidInput)?];
    let dtls_handshake_config = Arc::new(
        dtls::config::ConfigBuilder::default()
            .with_certificates(
                certificates
                    .iter()
                    .map(|c| c.dtls_certificate.clone())
                    .collect(),
            )
            .with_srtp_protection_profiles(vec![SrtpProtectionProfile::Srtp_Aes128_Cm_Hmac_Sha1_80])
            .with_extended_master_secret(dtls::config::ExtendedMasterSecretType::Require)
            .build(false, None)
            .map_err(|_| std::io::ErrorKind::InvalidInput)?,
    );
    let sctp_endpoint_config = Arc::new(sctp::EndpointConfig::default());
    let sctp_server_config = Arc::new(sctp::ServerConfig::default());
    let server_config = Arc::new(
        ServerConfig::new(certificates)
            .with_dtls_handshake_config(dtls_handshake_config)
            .with_sctp_endpoint_config(sctp_endpoint_config)
            .with_sctp_server_config(sctp_server_config)
            .with_idle_timeout(Duration::from_secs(30)),
    );
    let (stop_meter_tx, stop_meter_rx) = async_broadcast::broadcast::<()>(1);
    let wait_group = WaitGroup::new();
    let meter_provider = init_meter_provider(stop_meter_rx, wait_group.clone());

    for port in media_ports {
        let worker = wait_group.add(1);
        let stop_rx = stop_rx.clone();
        let (signaling_tx, signaling_rx) = mpsc::sync_channel(1);

        // Spin up a UDP socket for the RTC. All WebRTC traffic is going to be multiplexed over this single
        // server socket. Clients are identified via their respective remote (UDP) socket address.
        let socket = UdpSocket::bind(format!("{host_addr}:{port}"))
            .expect(&format!("binding to {host_addr}:{port}"));

        media_port_thread_map.insert(port, signaling_tx);
        let server_config = server_config.clone();
        let meter_provider = meter_provider.clone();
        // The run loop is on a separate thread to the web server.
        std::thread::spawn(move || {
            if let Err(err) = sync_run(stop_rx, socket, signaling_rx, server_config, meter_provider)
            {
                eprintln!("run_sfu got error: {}", err);
            }
            worker.done();
        });
    }

    // let media_port_thread_map = Arc::new(media_port_thread_map);
    let signal_port = cli.signal_port;

    web_server::start(
        &host_addr.to_string(),
        &signal_port.to_string(),
        media_port_thread_map.clone(),
    )
    .await?;

    // let (signal_handle, signal_cancel_tx) = if cli.force_local_loop {
    //     // for integration test, no ssl
    //     let signal_server = Server::new(format!("{}:{}", host_addr, signal_port), move |request| {
    //         web_request(request, media_port_thread_map.clone())
    //     })
    //     .expect("starting the signal server");

    //     let port = signal_server.server_addr().port();
    //     info!("Connect a browser to https://{}:{}", host_addr, port);

    //     signal_server.stoppable()
    // } else {
    //     let signal_server = Server::new_ssl(
    //         format!("{}:{}", host_addr, signal_port),
    //         move |request| web_request(request, media_port_thread_map.clone()),
    //         certificate,
    //         private_key,
    //     )
    //     .expect("starting the signal server");

    //     let port = signal_server.server_addr().port();
    //     info!("Connect a browser to https://{}:{}", host_addr, port);

    //     signal_server.stoppable()
    // };

    info!("Press Ctrl-C to stop");
    std::thread::spawn(move || {
        let mut stop_tx = Some(stop_tx);
        let mut stop_meter_tx = Some(stop_meter_tx);
        ctrlc::set_handler(move || {
            if let Some(stop_meter_tx) = stop_meter_tx.take() {
                let _ = stop_meter_tx.try_broadcast(());
            }
            if let Some(stop_tx) = stop_tx.take() {
                let _ = stop_tx.send(());
            }
        })
        .expect("Error setting Ctrl-C handler");
    });
    let _ = stop_rx.recv();
    info!("Wait for Signaling Sever and Media Server Gracefully Shutdown...");
    wait_group.wait();
    // let _ = signal_cancel_tx.send(());
    // let _ = signal_handle.join();

    Ok(())
}
