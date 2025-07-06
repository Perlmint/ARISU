use anyhow::Context as _;
use ironrdp::server::{Credentials, RdpServer, TlsIdentityCtx};
use std::{net::IpAddr, str::FromStr};
use tokio::sync::{mpsc, oneshot, watch};
use tracing::{error, info};

use crate::{config::Config, counter::IntervalCounter, screen::ScreenCapture};

#[derive(Debug)]
pub enum ServerCommand {
    Start(Config),
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Stopped,
    Starting,
    Running,
    Stopping,
    Error,
}

pub struct ServerController {
    command_sender: mpsc::UnboundedSender<ServerCommand>,
    status_receiver: watch::Receiver<ServerStatus>,
}

impl ServerController {
    pub fn new(
        capture_counter: IntervalCounter,
        display_send_counter: IntervalCounter,
    ) -> anyhow::Result<Self> {
        let (command_sender, command_receiver) = mpsc::unbounded_channel();
        let (status_sender, status_receiver) = watch::channel(ServerStatus::Stopped);

        let server_manager = ServerManager::new(command_receiver, status_sender);

        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("Failed to build tokio runtime");

            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                rt.block_on(async move {
                    let local_set = tokio::task::LocalSet::new();
                    local_set
                        .run_until(async move {
                            server_manager
                                .run(capture_counter, display_send_counter)
                                .await;
                        })
                        .await;
                });
            }));

            if let Err(panic_payload) = result {
                error!("ServerManager panicked: {:?}", panic_payload);
                std::process::exit(1);
            }
        });

        Ok(Self {
            command_sender,
            status_receiver,
        })
    }

    pub fn start_server(&self, config: Config) -> anyhow::Result<()> {
        self.command_sender
            .send(ServerCommand::Start(config))
            .context("Failed to send start command")
    }

    pub fn stop_server(&self) -> anyhow::Result<()> {
        self.command_sender
            .send(ServerCommand::Stop)
            .context("Failed to send stop command")
    }

    pub fn get_status_sync(&self) -> ServerStatus {
        *self.status_receiver.borrow()
    }
}

struct ServerManager {
    command_receiver: mpsc::UnboundedReceiver<ServerCommand>,
    status_sender: watch::Sender<ServerStatus>,
    shutdown_sender: Option<oneshot::Sender<()>>,
}

impl ServerManager {
    fn new(
        command_receiver: mpsc::UnboundedReceiver<ServerCommand>,
        status_sender: watch::Sender<ServerStatus>,
    ) -> Self {
        Self {
            command_receiver,
            status_sender,
            shutdown_sender: None,
        }
    }

    async fn run(
        mut self,
        capture_counter: IntervalCounter,
        display_send_counter: IntervalCounter,
    ) {
        info!("ServerManager started, waiting for commands");
        while let Some(command) = self.command_receiver.recv().await {
            info!("ServerManager received command: {:?}", command);
            match command {
                ServerCommand::Start(config) => {
                    if matches!(
                        *self.status_sender.borrow(),
                        ServerStatus::Stopped | ServerStatus::Error
                    ) {
                        self.start_server_task(
                            config,
                            capture_counter.clone(),
                            display_send_counter.clone(),
                        )
                        .await;
                    }
                }
                ServerCommand::Stop => {
                    self.stop_server_task().await;
                }
            }
        }
        info!("ServerManager command loop ended");
    }

    async fn start_server_task(
        &mut self,
        config: Config,
        capture_counter: IntervalCounter,
        display_send_counter: IntervalCounter,
    ) {
        let _ = self.status_sender.send(ServerStatus::Starting);
        info!("Starting RDP server");

        let (shutdown_sender, shutdown_receiver) = oneshot::channel();
        self.shutdown_sender = Some(shutdown_sender);

        let status_sender = self.status_sender.clone();
        tokio::task::spawn_local(async move {
            let result = run_server(
                config,
                capture_counter,
                display_send_counter,
                shutdown_receiver,
            )
            .await;

            let final_status = match result {
                Ok(()) => {
                    info!("Server stopped gracefully");
                    ServerStatus::Stopped
                }
                Err(e) => {
                    error!("Server error: {}", e);
                    ServerStatus::Error
                }
            };

            let _ = status_sender.send(final_status);
        });

        let _ = self.status_sender.send(ServerStatus::Running);
    }

    async fn stop_server_task(&mut self) {
        if matches!(*self.status_sender.borrow(), ServerStatus::Running) {
            let _ = self.status_sender.send(ServerStatus::Stopping);
            info!("Stopping RDP server");

            if let Some(shutdown_sender) = self.shutdown_sender.take() {
                let _ = shutdown_sender.send(());
            }
        }
    }
}

async fn run_server(
    config: Config,
    capture_counter: IntervalCounter,
    display_send_counter: IntervalCounter,
    mut shutdown_receiver: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    info!("run_server: Starting");
    let mut local_set = tokio::task::LocalSet::new();

    // Always use hybrid security
    let host = "0.0.0.0";
    let port = 3389;

    info!("run_server: Building RDP server");
    let server_builder = RdpServer::builder().with_addr((IpAddr::from_str(host)?, port));

    info!("run_server: Initializing TLS identity");
    let server_builder = {
        let identity = TlsIdentityCtx::init_from_paths(&config.certificate, &config.key)
            .context("failed to init TLS identity")?;
        let acceptor = identity
            .make_acceptor()
            .context("failed to build TLS acceptor")?;

        server_builder.with_hybrid(acceptor, identity.pub_key)
    };

    info!("run_server: Creating display handler");
    let (screen_handler, screen_job_processor) =
        match ScreenCapture::new(&local_set, capture_counter, display_send_counter) {
            Ok(result) => {
                info!("run_server: ScreenCapture created successfully");
                result
            }
            Err(e) => {
                error!("run_server: Failed to create ScreenCapture: {}", e);
                return Err(e);
            }
        };

    info!("run_server: Building server with handlers");
    let mut server = server_builder
        .with_input_handler(screen_handler.input_handler())
        .with_display_handler(screen_handler.clone())
        .build();

    info!("run_server: Setting credentials");
    server.set_credentials(Some(Credentials {
        username: config.auth_id,
        password: config.auth_password,
        domain: None,
    }));

    info!("run_server: Spawning server task");
    let (server_shutdown_sender, mut server_shutdown_receiver) = oneshot::channel::<()>();
    let server_task = local_set.spawn_local(async move {
        info!("run_server: About to call server.run()");
        tokio::select! {
            result = server.run() => {
                if let Err(e) = result {
                    error!(?e, "Server run error");
                }
                info!("run_server: server.run() completed");
            }
            _ = &mut server_shutdown_receiver => {
                info!("run_server: Server received shutdown signal, stopping");
                // Server should stop when shutdown signal is received
            }
        }
    });

    info!("run_server: Entering select loop with LocalSet");
    // Wait for either the server to complete or shutdown signal
    let shutdown_initiated = tokio::select! {
        _result = &mut local_set => {
            info!("run_server: LocalSet completed naturally");
            false
        }
        _ = &mut shutdown_receiver => {
            info!("run_server: Received shutdown signal, initiating graceful shutdown");
            screen_handler.shutdown().await;
            // Signal the server task to shut down gracefully
            let _ = server_shutdown_sender.send(());
            true
        }
    };

    // If shutdown was initiated, wait for graceful completion with timeout
    let abort = if shutdown_initiated {
        info!("run_server: Waiting for server to shut down gracefully (60s timeout)");
        tokio::select! {
            _ = &mut local_set => {
                info!("run_server: Server shut down gracefully");
                false
            }
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => {
                error!("run_server: Graceful shutdown timeout, aborting server task");
                true
            }
        }
    } else {
        false
    };
    if abort {
        server_task.abort();
        // Still need to wait for LocalSet to complete after abort
        local_set.await;
    }

    info!("run_server: Awaiting screen_job_processor");
    screen_job_processor
        .await
        .context("display job join error")
        .and_then(|i| i.context("display job error"))?;

    info!("run_server: Completed successfully");
    Ok(())
}
