use dispatch2::DispatchQueue;
use ironrdp::{
    rdpsnd::pdu::{AudioFormat, ClientAudioFormatPdu, WaveFormat},
    server::{
        RdpsndServerHandler, RdpsndServerMessage, ServerEvent, ServerEventSender,
        SoundServerFactory,
    },
};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread as _,
    DefinedClass as _,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{SCStream, SCStreamOutput, SCStreamOutputType};
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;

use anyhow::anyhow;
use std::sync::{Arc, RwLock};

use super::{ScreenCapture, ScreenJob};

pub const SAMPLE_RATE: u32 = 48000;
pub const BITS_PER_SAMPLE: u16 = 32;
pub const CHANNELS: u16 = 2;

/// Extracts PCM audio data from a CMSampleBuffer
fn extract_pcm_from_sample_buffer(sample_buffer: &CMSampleBuffer) -> anyhow::Result<Vec<u8>> {
    let num_samples = unsafe { sample_buffer.num_samples() };
    if num_samples == 0 {
        return Err(anyhow!("Sample buffer contains no samples"));
    }

    // Get the data buffer from the sample buffer
    let Some(data_buffer) = (unsafe { sample_buffer.data_buffer() }) else {
        return Err(anyhow!("No data buffer in sample buffer"));
    };

    // Get the length and raw data pointer from the block buffer
    let data_length = unsafe { data_buffer.data_length() };
    let mut data_ptr = std::ptr::null_mut::<i8>();
    let mut _length_at_offset_out = 0;
    let mut _total_length_out = 0;

    let status = unsafe {
        data_buffer.data_pointer(
            0, // offset
            &mut _length_at_offset_out,
            &mut _total_length_out,
            &mut data_ptr,
        )
    };

    if status != 0 {
        return Err(anyhow!(
            "Failed to get data pointer from block buffer: {}",
            status
        ));
    }

    if data_ptr.is_null() {
        return Err(anyhow!("Data pointer is null"));
    }

    // SAFETY: CoreMedia guarantees the data pointer is valid for the specified length
    let audio_data =
        unsafe { std::slice::from_raw_parts(data_ptr as *const u8, data_length as usize) };

    tracing::debug!(
        "Extracted {} bytes of PCM data from {} samples",
        audio_data.len(),
        num_samples
    );

    Ok(audio_data.to_vec())
}

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
    #[allow(dead_code)]
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
            tracing::info!("sound candidate - {fmt:?}");
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
            n_block_align: (CHANNELS * BITS_PER_SAMPLE / 8) as u16, // 4 bytes per sample
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

struct AudioCaptureDelegateIvars {
    sender: Arc<RwLock<Option<UnboundedSender<ServerEvent>>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = AudioCaptureDelegateIvars]
    struct AudioCaptureDelegate;

    unsafe impl NSObjectProtocol for AudioCaptureDelegate {}

    unsafe impl SCStreamOutput for AudioCaptureDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if of_type != SCStreamOutputType::Audio {
                tracing::warn!("Received non-audio sample buffer type: {:?}", of_type);
                return;
            }

            if let Err(e) = Self::handle_stream(self.ivars(), sample_buffer) {
                tracing::error!("Error handling audio stream: {e:?}");
            }
        }
    }
);

impl AudioCaptureDelegate {
    fn new(sender: Arc<RwLock<Option<UnboundedSender<ServerEvent>>>>) -> Retained<Self> {
        let this = AudioCaptureDelegate::alloc();
        let this = this.set_ivars(AudioCaptureDelegateIvars { sender });
        unsafe { msg_send![super(this), init] }
    }

    fn handle_stream(
        ivars: &AudioCaptureDelegateIvars,
        sample_buffer: &CMSampleBuffer,
    ) -> anyhow::Result<()> {
        // Get the number of samples
        let num_samples = unsafe { sample_buffer.num_samples() };
        if num_samples == 0 {
            return Err(anyhow!("Sample buffer contains no samples"));
        }

        let Some(_format_description) = (unsafe { sample_buffer.format_description() }) else {
            return Err(anyhow!("No format description in sample buffer"));
        };

        // Extract PCM data from the sample buffer
        let pcm_data = extract_pcm_from_sample_buffer(sample_buffer)?;

        // Get the presentation timestamp from the sample buffer
        let presentation_time = unsafe { sample_buffer.presentation_time_stamp() };
        let timestamp_ms =
            (presentation_time.value as f64 / presentation_time.timescale as f64 * 1000.0) as u32;

        tracing::debug!(
            "Audio sample timestamp: {}ms (value: {}, timescale: {})",
            timestamp_ms,
            presentation_time.value,
            presentation_time.timescale
        );

        // Send the PCM data to RDP client
        if let Some(sender) = ivars.sender.write().unwrap().as_ref() {
            sender
                .send(ServerEvent::Rdpsnd(RdpsndServerMessage::Wave(
                    pcm_data,
                    timestamp_ms,
                )))
                .map_err(|e| anyhow!("Failed to send audio data: {e:?}"))?;
        } else {
            return Err(anyhow!("No RDP sender available"));
        }

        Ok(())
    }
}

impl super::ScreenCaptureContext {
    pub(crate) fn handle_sound_job(&mut self, job: Job) {
        match job {
            Job::Start => {
                let dispatch_queue = DispatchQueue::new("app.perlmint.arisu.sound", None);
                let delegate = AudioCaptureDelegate::new(self.rdp_event_sender.clone());
                let output = ProtocolObject::from_retained(delegate);
                tracing::info!("sound start");
                let ret = unsafe {
                    self.stream.addStreamOutput_type_sampleHandlerQueue_error(
                        &output,
                        SCStreamOutputType::Audio,
                        Some(&dispatch_queue),
                    )
                };
                if let Err(e) = ret {
                    tracing::error!("Failed to start audio capture: {e}");
                } else {
                    self.audio_delegate = Some((output, dispatch_queue.into()));
                }
            }
            Job::Stop => {
                if let Some((delegate, _)) = self.audio_delegate.take() {
                    tracing::info!("sound stop");
                    if let Err(e) = unsafe {
                        self.stream
                            .removeStreamOutput_type_error(&delegate, SCStreamOutputType::Audio)
                    } {
                        tracing::error!("Failed to stop audio capture: {e}");
                    }
                } else {
                    tracing::warn!("No audio delegate found to stop");
                }
            }
        }
    }
}
