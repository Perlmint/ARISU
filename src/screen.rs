use ironrdp::server::ServerEvent;
use objc::runtime::Object;
use screencapturekit::{
    shareable_content::{SCShareableContent},
    stream::{
        configuration::{pixel_format::PixelFormat, SCStreamConfiguration},
        content_filter::SCContentFilter,
        SCStream,
    },
};
use std::sync::{Arc, RwLock};
use tokio::{
    sync::{mpsc, watch},
    task::{JoinHandle, LocalSet},
};

use crate::{counter::IntervalCounter, input::InputHandler};

mod display;

mod sound;

#[derive(Clone, Copy)]
struct ScreenOutputIndex(usize);

impl ScreenOutputIndex {
    fn new(val: *mut Object) -> Self {
        Self(val as usize)
    }

    fn to_raw(self) -> *mut Object {
        self.0 as *mut _
    }
}

enum ScreenJob {
    Display(display::Job),
    Sound(sound::Job),
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
    counter: IntervalCounter,
    screen_size: watch::Receiver<ScreenSize>,
}

struct ScreenCaptureContext {
    job_sender: mpsc::Sender<ScreenJob>,
    display_size: watch::Sender<ScreenSize>,
    rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>>,
    counter: IntervalCounter,
    stream: SCStream,
}

impl ScreenCapture {
    pub fn new(
        main_thread_local_set: &LocalSet,
        capture_counter: IntervalCounter,
        display_send_counter: IntervalCounter,
    ) -> anyhow::Result<(Self, JoinHandle<anyhow::Result<()>>)> {
        let config = SCStreamConfiguration::new()
            .set_captures_audio(true)
            .map_err(|e| anyhow::anyhow!("Failed to setCapturesAudio - {e:?}"))?
            // .set_sample_rate(sound::SAMPLE_RATE as _)
            // .map_err(|e| anyhow::anyhow!("Failed to setSampleRate - {e:?}"))?
            .set_channel_count(sound::CHANNELS as _)
            .map_err(|e| anyhow::anyhow!("Failed to setChannelCount - {e:?}"))?
            .set_pixel_format(PixelFormat::BGRA)
            .map_err(|e| anyhow::anyhow!("Failed setPixelFormat - {e:?}"))?;
        let screen_chnnal = mpsc::channel::<ScreenJob>(10);
        let display = {
            let shareable_content = SCShareableContent::get()
                .map_err(|e| anyhow::anyhow!("Failed to get SCShareableContent - {e:?}"))?;
            let mut displays = shareable_content.displays();
            displays.swap_remove(0)
        };

        let rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>> =
            Default::default();

        let filter = SCContentFilter::new().with_display_excluding_applications_excepting_windows(
            &display,
            &[],
            &[],
        );
        let width = display.width() as u16;
        let height = display.height() as u16;
        tracing::info!("screen initial size - width: {width}, height: {height}");
        let (display_size, screen_size) = watch::channel(ScreenSize {
            client: (width, height),
            server: (width, height),
        });
        let stream = SCStream::new(&filter, &config);
        stream
            .start_capture()
            .map_err(|e| anyhow::anyhow!("Failed to start capture - {e:?}"))?;

        let mut context = ScreenCaptureContext {
            job_sender: screen_chnnal.0.clone(),
            rdp_event_sender: rdp_event_sender.clone(),
            counter: capture_counter,
            display_size,
            stream,
        };
        let handle = main_thread_local_set.spawn_local(async move {
            let mut job_receiver = screen_chnnal.1;

            tracing::info!("Display handling loop started");

            while let Some(job) = job_receiver.recv().await {
                tracing::debug!("Received display job");
                match job {
                    ScreenJob::Display(job) => context.handle_display_job(job),
                    ScreenJob::Sound(job) => context.handle_sound_job(job),
                }
            }

            tracing::info!("Display handler stopped");

            Ok(())
        });

        Ok((
            Self {
                job_sender: screen_chnnal.0,
                rdp_event_sender,
                counter: display_send_counter,
                screen_size,
            },
            handle,
        ))
    }

    pub fn input_handler(&self) -> InputHandler {
        InputHandler::new(self.screen_size.clone())
    }
}
