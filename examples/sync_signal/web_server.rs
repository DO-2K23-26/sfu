use std::{collections::HashMap, sync::mpsc::SyncSender};

use actix_web::{web::Data, App, HttpServer};
use stun::addr;
use tracing::info;

use crate::{signaling_controller::{handle_offer, index, leave}, SignalingMessage};

pub async fn start(addr: &str, port: &str, media_port_thread_map: HashMap<u16, SyncSender<SignalingMessage>>) -> std::io::Result<()> {
    let addr = format!("{}:{}", addr, port);
    let mut builder = openssl::ssl::SslAcceptor::mozilla_intermediate(openssl::ssl::SslMethod::tls()).unwrap();
    builder.set_private_key_file("examples/util/key.pem", openssl::ssl::SslFiletype::PEM).unwrap();
    builder.set_certificate_chain_file("examples/util/cer.pem").unwrap();


    info!("Starting web server at {}", addr);
    HttpServer::new(move || {
        App::new()
            .app_data(Data::new(media_port_thread_map.clone()))
            .service(handle_offer)
            .service(index)
            .service(leave)
    })
    .bind_openssl(addr, builder)?
    .run()
    .await
}