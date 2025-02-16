use ironrdp::{
    rdpsnd::pdu::{AudioFormat, ClientAudioFormatPdu, WaveFormat},
    server::{
        RdpsndServerHandler, RdpsndServerMessage, ServerEvent, ServerEventSender,
        SoundServerFactory,
    },
};
use screencapturekit::stream::{
    output_trait::SCStreamOutputTrait, output_type::SCStreamOutputType,
};
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, RwLock,
};

use super::{ScreenCapture, ScreenJob};

pub const SAMPLE_RATE: u32 = 48000;
pub const BITS_PER_SAMPLE: u16 = 32;
pub const CHANNELS: u16 = 1;

pub(crate) enum Job {
    Start,
    Stop,
}

impl ServerEventSender for ScreenCapture {
    fn set_sender(&mut self, sender: UnboundedSender<ServerEvent>) {
        let mut inner = self
            .rdp_event_sender
            .write()
            .expect("Failed to retrieve write lock");
        tracing::info!("set rdp sender");

        *inner = Some(sender);
    }
}

#[derive(Debug)]
struct SoundServer {
    job_sender: mpsc::Sender<ScreenJob>,
    rdp_event_sender: Arc<RwLock<Option<mpsc::UnboundedSender<ServerEvent>>>>,
}

impl SoundServerFactory for ScreenCapture {
    fn build_backend(&self) -> Box<dyn RdpsndServerHandler> {
        Box::new(SoundServer {
            job_sender: self.job_sender.clone(),
            rdp_event_sender: self.rdp_event_sender.clone(),
        })
    }
}

impl SoundServer {
    fn choose_format(&self, client_formats: &[AudioFormat]) -> Option<u16> {
        for (n, fmt) in client_formats.iter().enumerate() {
            tracing::info!("candidate - {fmt:?}");
            if self.get_formats().contains(fmt) {
                return u16::try_from(n).ok();
            }
        }
        None
    }
}

impl RdpsndServerHandler for SoundServer {
    fn get_formats(&self) -> &[AudioFormat] {
        tracing::info!("get sound format");

        &[AudioFormat {
            format: WaveFormat::PCM,
            n_channels: CHANNELS,
            n_samples_per_sec: SAMPLE_RATE,
            n_avg_bytes_per_sec: SAMPLE_RATE * (CHANNELS * BITS_PER_SAMPLE) as u32 / 8,
            n_block_align: 16 * 8, // 16 bytes
            bits_per_sample: BITS_PER_SAMPLE,
            data: None,
        }]
    }

    fn start(&mut self, client_format: &ClientAudioFormatPdu) -> Option<u16> {
        let Some(format_idx) = self.choose_format(&client_format.formats) else {
            return Some(0);
        };
        let _ = self.job_sender.try_send(ScreenJob::Sound(Job::Start));
        Some(format_idx)
    }

    fn stop(&mut self) {
        let _ = self.job_sender.try_send(ScreenJob::Sound(Job::Stop));
    }
}

struct AudioCaptureDelegate {
    sender: Arc<RwLock<Option<UnboundedSender<ServerEvent>>>>,
    ts: AtomicU32,
}

impl SCStreamOutputTrait for AudioCaptureDelegate {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: screencapturekit::output::CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) {
        if of_type != SCStreamOutputType::Audio {
            return;
        }

        let Ok(audio_buffer_list) = sample_buffer
            .get_audio_buffer_list()
            .map_err(|e| tracing::error!("Failed to get audio buffer: {e:?}"))
        else {
            return;
        };
        let Some(buffer) = audio_buffer_list.get(0) else {
            return;
        };
        let data = buffer.data();

        let sender = self.sender.write().unwrap();
        if let Some(sender) = sender.as_ref() {
            let _ = sender.send(ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(
                data.to_vec(),
                self.ts.load(Ordering::SeqCst),
            )));
        }
        self.ts.fetch_add(100, Ordering::SeqCst);
    }
}

impl super::ScreenCaptureContext {
    pub(crate) fn handle_sound_job(&mut self, job: Job) {
        match job {
            Job::Start => {
                let delegate = AudioCaptureDelegate {
                    sender: self.rdp_event_sender.clone(),
                    ts: AtomicU32::new(0),
                };
                tracing::info!("sound start");
                self.stream
                    .add_output_handler(delegate, SCStreamOutputType::Audio);
            }
            Job::Stop => {
                tracing::info!("sound stop");
                // self.stream.remove_output_handler(index, SCStreamOutputType::Audio);
            }
        }
    }
}
