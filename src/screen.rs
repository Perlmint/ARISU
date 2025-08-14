use anyhow::Context as _;
use dispatch2::DispatchQueue;
use ironrdp::server::ServerEvent;
use objc2::{rc::Retained, runtime::ProtocolObject, AnyThread as _};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_video::kCVPixelFormatType_32BGRA;
use objc2_foundation::{NSArray, NSError};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCCaptureResolutionType, SCContentFilter, SCShareableContent, SCStream,
    SCStreamConfiguration, SCStreamOutput,
};
use std::sync::{Arc, RwLock};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::{JoinHandle, LocalSet},
};

use crate::{counter::IntervalCounter, input::InputHandler};

mod display;
mod sound;

enum ScreenJob {
    Display(display::Job),
    Sound(sound::Job),
    Shutdown,
}

#[derive(Debug, Clone, Copy)]
pub struct ScreenSize {
    pub client: (u16, u16),
    pub server: (u16, u16),
}

#[derive(Clone)]
pub struct ScreenCapture {
    job_sender: mpsc::Sender<ScreenJob>,
    rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>>,
    screen_size: watch::Receiver<ScreenSize>,
}

struct ScreenCaptureContext {
    job_sender: mpsc::Sender<ScreenJob>,
    display_size: watch::Sender<ScreenSize>,
    rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>>,
    capture_counter: IntervalCounter,
    send_counter: IntervalCounter,
    stream: Retained<SCStream>,
    capture_config: Retained<SCStreamConfiguration>,
    display_delegate: Option<(
        Retained<ProtocolObject<dyn SCStreamOutput>>,
        Retained<DispatchQueue>,
    )>,
    audio_delegate: Option<(
        Retained<ProtocolObject<dyn SCStreamOutput>>,
        Retained<DispatchQueue>,
    )>,
}

impl ScreenCapture {
    pub async fn new(
        main_thread_local_set: &LocalSet,
        capture_counter: IntervalCounter,
        display_send_counter: IntervalCounter,
    ) -> anyhow::Result<(Self, JoinHandle<anyhow::Result<()>>)> {
        let screen_chnnal = mpsc::channel::<ScreenJob>(10);
        let display = {
            let (tx, rx) = oneshot::channel();
            let block = block2::RcBlock::new({
                let tx = std::cell::Cell::new(Some(tx));
                move |shareable_content: *mut SCShareableContent, _error: *mut NSError| {
                    let shareable_content = unsafe { &mut *shareable_content };
                    let displays = unsafe { shareable_content.displays() }
                        .firstObject()
                        .ok_or(anyhow::anyhow!("No displays found"));
                    if tx.take().unwrap().send(displays).is_err() {
                        tracing::error!("Failed to send shareable content");
                    }
                }
            });
            unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
            rx.await.context("Failed to get shareable content")??
        };

        let rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>> =
            Default::default();

        let filter = unsafe {
            SCContentFilter::initWithDisplay_excludingApplications_exceptingWindows(
                SCContentFilter::alloc(),
                &display,
                &NSArray::new(),
                &NSArray::new(),
            )
        };
        let width = unsafe { display.width() as u16 };
        let height = unsafe { display.height() as u16 };
        tracing::info!("screen initial size - width: {width}, height: {height}");

        let config = unsafe { SCStreamConfiguration::new() };
        unsafe {
            config.setCapturesAudio(true);
            config.setSampleRate(sound::SAMPLE_RATE as _);
            config.setChannelCount(sound::CHANNELS as _);
            config.setPixelFormat(kCVPixelFormatType_32BGRA);
            config.setCaptureResolution(SCCaptureResolutionType::Best);
            config.setWidth(width as _);
            config.setHeight(height as _);
            config.setSourceRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as _, height as _)));
            config.setDestinationRect(CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(width as _, height as _)));
            config.setPreservesAspectRatio(true);
            config.setCaptureDynamicRange(SCCaptureDynamicRange::SDR);
        }

        let (display_size, screen_size) = watch::channel(ScreenSize {
            client: (width, height),
            server: (width, height),
        });
        let stream = unsafe {
            SCStream::initWithFilter_configuration_delegate(
                SCStream::alloc(),
                &filter,
                &config,
                None,
            )
        };
        unsafe { stream.startCaptureWithCompletionHandler(None) };

        let mut context = ScreenCaptureContext {
            job_sender: screen_chnnal.0.clone(),
            rdp_event_sender: rdp_event_sender.clone(),
            capture_counter,
            send_counter: display_send_counter,
            display_size,
            capture_config: config,
            stream,
            display_delegate: None,
            audio_delegate: None,
        };
        let handle = main_thread_local_set.spawn_local(async move {
            let mut job_receiver = screen_chnnal.1;

            tracing::info!("Display handling loop started");

            while let Some(job) = job_receiver.recv().await {
                tracing::debug!("Received display job");
                match job {
                    ScreenJob::Display(job) => context.handle_display_job(job),
                    ScreenJob::Sound(job) => context.handle_sound_job(job),
                    ScreenJob::Shutdown => {
                        tracing::info!("Received shutdown signal, stopping ScreenCapture");
                        context.shutdown();
                        break;
                    }
                }
            }

            tracing::info!("Display handler stopped");

            Ok(())
        });

        Ok((
            Self {
                job_sender: screen_chnnal.0,
                rdp_event_sender,
                screen_size,
            },
            handle,
        ))
    }

    pub fn input_handler(&self) -> InputHandler {
        InputHandler::new(self.screen_size.clone())
    }

    pub async fn shutdown(&self) {
        if let Err(e) = self.job_sender.send(ScreenJob::Shutdown).await {
            tracing::error!("Failed to send shutdown signal to ScreenCapture: {}", e);
        }
    }
}

impl ScreenCaptureContext {
    fn shutdown(&mut self) {
        tracing::info!("Shutting down ScreenCaptureContext");

        // Clear RDP event sender to prevent further audio processing
        if let Ok(mut sender) = self.rdp_event_sender.write() {
            *sender = None;
        }

        unsafe { self.stream.stopCaptureWithCompletionHandler(None) };
    }
}
