use std::{net::IpAddr, path::PathBuf, str::FromStr};

use anyhow::Context as _;
use clap::Parser;
// use clipboard::StubCliprdrServerFactory;
use counter::IntervalCounter;
use ironrdp::server::{Credentials, RdpServer, TlsIdentityCtx};
use screen::ScreenCapture;
use strum::EnumString;
use tracing::error;

mod counter;

// mod clipboard;
// mod credential;
mod input;
mod screen;

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
#[strum(ascii_case_insensitive)]
enum Security {
    None,
    Tls,
    Hybrid,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Name of the person to greet
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 3389)]
    port: u16,
    #[arg(long)]
    certificate: Option<PathBuf>,
    #[arg(long)]
    key: Option<PathBuf>,
    #[arg(long, default_value = "none")]
    security: Security,
}

#[cfg(feature = "gui")]
mod gui;
#[cfg(feature = "gui")]
use gui::App;

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let top_local_set = tokio::task::LocalSet::new();
    let mut join_set = tokio::task::JoinSet::new();

    let args = Args::parse();

    let capture_counter = IntervalCounter::new();
    let display_send_counter = IntervalCounter::new();

    use tracing_subscriber::{filter::LevelFilter, fmt, EnvFilter};
    fmt()
        .with_max_level(LevelFilter::INFO)
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let _server_handle = join_set.spawn_local_on(
        async move {
            let local_set = tokio::task::LocalSet::new();
            let security = args.security;

            tracing::info!("Building RDP server");
            let server_builder =
                RdpServer::builder().with_addr((IpAddr::from_str(&args.host)?, args.port));

            let server_builder = if let Some((cert_path, key_path)) = args.certificate.zip(args.key)
            {
                let identity = TlsIdentityCtx::init_from_paths(&cert_path, &key_path)
                    .context("failed to init TLS identity")?;
                let acceptor = identity
                    .make_acceptor()
                    .context("failed to build TLS acceptor")?;

                if security == Security::Hybrid {
                    server_builder.with_hybrid(acceptor, identity.pub_key)
                } else {
                    server_builder.with_tls(acceptor)
                }
            } else if security == Security::None {
                server_builder.with_no_security()
            } else {
                anyhow::bail!("Security is specified. but cert is not specified");
            };

            tracing::info!("Create clipboard server");
            // let cliprdr = Box::new(StubCliprdrServerFactory::new());

            tracing::info!("Create display handler");
            let (screen_handler, screen_job_processor) =
                ScreenCapture::new(&local_set, capture_counter, display_send_counter)?;

            let mut server = server_builder
                .with_input_handler(screen_handler.input_handler())
                .with_display_handler(screen_handler.clone())
                // .with_cliprdr_factory(Some(cliprdr))
                // .with_sound_factory(Some(Box::new(screen_handler)))
                .build();

            server.set_credentials(Some(Credentials {
                username: "user".to_string(),
                password: "user".to_string(),
                domain: None,
            }));

            let server_join_handler = local_set.spawn_local(async move {
                tracing::info!("Starting server");
                if let Err(e) = server.run().await {
                    tracing::error!(?e, "Server run error");
                }
            });

            local_set.await;
            server_join_handler.await.context("server error")?;
            screen_job_processor
                .await
                .context("display job join error")
                .and_then(|i| i.context("diaply job error"))?;

            Ok(())
        },
        &top_local_set,
    );
    join_set.spawn(async move {
        Result::<(), anyhow::Error>::Ok(())
    });

    tracing::info!("Start server");
    let (_, join_ret) = tokio::join!(top_local_set, join_set.join_all(),);
    for ret in join_ret {
        if let Err(e) = ret {
            error!(?e);
        }
    }

    Ok(())
}
