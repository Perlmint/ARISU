use anyhow::Context as _;
use bytes::Bytes;
use ironrdp::server::{
    BitmapUpdate, DesktopSize, DisplayUpdate, RdpServerDisplay, RdpServerDisplayUpdates,
};
use screencapturekit::{
    output::{
        sc_stream_frame_info::{SCFrameStatus, SCStreamFrameInfo},
        CVPixelBuffer, LockTrait,
    },
    stream::{output_trait::SCStreamOutputTrait, output_type::SCStreamOutputType},
};
use std::{cell::RefCell, num::NonZeroU16, sync::Arc};
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::{counter::IntervalCounter, screen::ScreenJob};

use super::{ScreenOutputIndex, ScreenSize};

pub(super) enum Job {
    GetSize(oneshot::Sender<(u16, u16)>),
    SetSize(u16, u16),
    CaptureStart(oneshot::Sender<anyhow::Result<DisplayUpdates>>),
    CaptureStop(ScreenOutputIndex),
}

#[derive(Debug, Clone)]
struct CapturedData {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    data: Vec<u8>,
}

pub(super) struct DisplayUpdates {
    index: ScreenOutputIndex,
    display_sender: mpsc::Sender<ScreenJob>,
    capture_receiver: triple_buffer::Output<CapturedData>,
    display_size: watch::Receiver<ScreenSize>,
    update_notification: Arc<Notify>,
    send_counter: IntervalCounter,
}

impl Drop for DisplayUpdates {
    fn drop(&mut self) {
        let _ = self
            .display_sender
            .try_send(ScreenJob::Display(Job::CaptureStop(self.index)));
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for DisplayUpdates {
    async fn next_update(&mut self) -> Option<DisplayUpdate> {
        self.update_notification.notified().await;
        self.capture_receiver.update();
        let CapturedData {
            x,
            y,
            width,
            height,
            data: buffer,
        } = self.capture_receiver.peek_output_buffer();
        tracing::trace!(
            "Received display update: ({x}, {y}) {width} x {height}, buffer size: {}, {}, {:?}",
            buffer.len(),
            if buffer.iter().all(|&b| b == 0) {
                "black"
            } else {
                "data"
            },
            buffer.as_ptr()
        );
        self.send_counter.update();
        Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x: *x,
            y: *y,
            width: unsafe { NonZeroU16::new_unchecked(*width as u16) },
            height: unsafe { NonZeroU16::new_unchecked(*height as u16) },
            format: ironrdp::server::PixelFormat::BgrA32,
            data: Bytes::from_static(unsafe { &*(buffer.as_slice() as *const [u8]) }),
            stride: (4 * width) as usize,
        }))
    }
}

#[async_trait::async_trait]
impl RdpServerDisplay for super::ScreenCapture {
    async fn size(&mut self) -> DesktopSize {
        let (sender, receiver) = oneshot::channel();
        self.job_sender
            .send(ScreenJob::Display(Job::GetSize(sender)))
            .await
            .unwrap_or_else(|e| panic!("Failed to send display job to main thread - {e:?}"));
        let (width, height) = receiver
            .await
            .unwrap_or_else(|e| panic!("Failed to get display size - {e:?}"));
        tracing::info!("init size: {width} x {height}");
        DesktopSize {
            width: width as u16,
            height: height as u16,
        }
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        let (sender, receiver) = oneshot::channel();
        self.job_sender
            .send(ScreenJob::Display(Job::CaptureStart(sender)))
            .await?;
        tracing::info!("Starting capture requested");
        let received = receiver.await??;

        Ok(Box::new(received))
    }

    fn request_layout(
        &mut self,
        layout: ironrdp::displaycontrol::pdu::DisplayControlMonitorLayout,
    ) {
        for layout in layout.monitors().iter() {
            let (width, height) = layout.dimensions();
            let device_scale_factor = layout.device_scale_factor();
            let desktop_scale_factor = layout.desktop_scale_factor();
            tracing::info!(?width, ?height, ?device_scale_factor, ?desktop_scale_factor);
            if let Err(e) = self
                .job_sender
                .try_send(ScreenJob::Display(Job::SetSize(width as _, height as _)))
            {
                tracing::error!("Failed to send display size job: {e:?}");
            }
        }
    }
}

fn convert_buffer(
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    input: &CVPixelBuffer,
    output: &mut CapturedData,
) -> bool {
    let plane_count = input.get_plane_count();
    let Ok(locked) = input
        .lock()
        .map_err(|e| tracing::error!("Failed to lock buffer - {e:?}"))
    else {
        return false;
    };
    let (base_address, bytes_per_row) = if plane_count == 0 {
        (locked.as_slice().as_ptr(), input.get_bytes_per_row())
    } else {
        (
            locked.as_slice_plane(0).as_ptr(),
            input.get_bytes_per_row_of_plane(0),
        )
    };
    let data_size = width * height * 4; // 4 bytes per pixel (BGRA)
    if output.data.len() < data_size {
        let reserve_size = data_size - output.data.len();
        tracing::trace!("reserve: {reserve_size}");
        output.data.reserve(reserve_size);
    }
    unsafe {
        output.data.set_len(data_size);
    }
    let out_addr = output.data.as_mut_ptr();
    for rect_y in 0..height {
        let src_addr = unsafe { base_address.add((y + rect_y) * (bytes_per_row as usize) + x * 4) };
        let out_addr = unsafe { out_addr.add(rect_y * width * 4 + x * 4) };
        unsafe {
            std::ptr::copy_nonoverlapping(src_addr, out_addr, width * 4);
        }
    }

    output.x = x as _;
    output.y = y as _;
    output.width = width as _;
    output.height = height as _;

    true
}

struct DisplayCaptureDelegate {
    sender: RefCell<triple_buffer::Input<CapturedData>>,
    update_notifier: Arc<Notify>,
    capture_counter: RefCell<IntervalCounter>,
}

impl SCStreamOutputTrait for DisplayCaptureDelegate {
    fn did_output_sample_buffer(
        &self,
        sample_buffer: screencapturekit::output::CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) {
        if of_type != SCStreamOutputType::Screen {
            tracing::error!("non-screen received");
            return;
        }

        let Ok(frame_info) = SCStreamFrameInfo::from_sample_buffer(&sample_buffer).map_err(|e| {
            tracing::error!("Failed to get frame info from sample buffer: {e:?}");
        }) else {
            return;
        };
        if frame_info.status() != SCFrameStatus::Complete {
            tracing::trace!("not completed");
            return;
        }
        let Some(dirty_rects) = frame_info.dirty_rects() else {
            tracing::error!("Failed to get dirty rects from frame info");
            return;
        };

        if let Some(pixel_buffer) = sample_buffer.get_pixel_buffer().ok() {
            let (mut x, mut y, max_x, max_y) =
                dirty_rects
                    .iter()
                    .fold((0, 0, 0, 0), |(min_x, min_y, max_x, max_y), rect| {
                        let x = rect.origin.x as usize;
                        let y = rect.origin.y as usize;
                        let width = rect.size.width as usize;
                        let height = rect.size.height as usize;

                        (
                            min_x.min(x),
                            min_y.min(y),
                            max_x.max(x + width),
                            max_y.max(y + height),
                        )
                    });
            let mut width = max_x - x;
            let mut height = max_y - y;
            if width == 0 || height == 0 {
                x = 0;
                y = 0;
                width = pixel_buffer.get_width() as usize;
                height = pixel_buffer.get_height() as usize;
            }
            let mut input_buffer = self.sender.borrow_mut();
            {
                let input_buffer = input_buffer.input_buffer_mut();
                if !convert_buffer(x, y, width, height, &pixel_buffer, input_buffer) {
                    tracing::error!("Failed to convert buffer");
                    return;
                };
            }
            input_buffer.publish();
            self.update_notifier.notify_waiters();
            self.capture_counter.borrow_mut().update();
        }
    }
}

impl super::ScreenCaptureContext {
    pub(crate) fn handle_display_job(&mut self, job: Job) {
        match job {
            Job::GetSize(sender) => {
                tracing::trace!("Requsted display size");
                let screen_size = *self.display_size.borrow();
                if let Err(e) = sender.send((screen_size.server.0, screen_size.server.1)) {
                    tracing::error!("Failed to send display size: {e:?}");
                }
            }
            Job::SetSize(width, height) => {
                // use objc2_core_graphics::{CGGetActiveDisplayList, CGDisplayCopyDisplayMode, CGDisplayMode, CGDirectDisplayID};
                // let mut active_displays = std::mem::MaybeUninit::<[CGDirectDisplayID; 1]>::uninit();
                // let mut display_count = std::mem::MaybeUninit::<u32>::uninit();
                // unsafe { CGGetActiveDisplayList(1, &raw mut (&mut *active_displays.as_mut_ptr())[0], display_count.as_mut_ptr()) };
                // let active_displays = unsafe { active_displays.assume_init() };
                // let display_count = unsafe { display_count.assume_init() };
                // if display_count == 0 {
                //     panic!("No active displays found");
                // }
                // let display = active_displays[0];
                // let display_mode = unsafe { CGDisplayCopyDisplayMode(display) }.unwrap();
                self.display_size.send_if_modified(|screen_size| {
                    if screen_size.client != (width, height) {
                        tracing::info!("Client display size changed: {} x {}", width, height);
                        screen_size.client = (width, height);
                        true
                    } else {
                        false
                    }
                });
            }
            Job::CaptureStart(sender) => {
                let screen_size = *self.display_size.borrow();
                let (capture_sender, capture_receiver) =
                    triple_buffer::triple_buffer(&CapturedData {
                        data: Vec::with_capacity(
                            (4 * screen_size.server.0 * screen_size.server.1) as usize,
                        ),
                        width: screen_size.server.0 as _,
                        height: screen_size.server.1 as _,
                        x: 0,
                        y: 0,
                    });
                let update_notification = Arc::new(Notify::new());
                let delegate = DisplayCaptureDelegate {
                    sender: RefCell::new(capture_sender),
                    update_notifier: update_notification.clone(),
                    capture_counter: RefCell::new(self.capture_counter.clone()),
                };
                let ret = self
                    .stream
                    .add_output_handler(delegate, SCStreamOutputType::Screen)
                    .context("Failed to start add stream output")
                    .map(|index| DisplayUpdates {
                        index: ScreenOutputIndex::new(index),
                        display_sender: self.job_sender.clone(),
                        update_notification,
                        capture_receiver,
                        display_size: self.display_size.subscribe(),
                        send_counter: self.send_counter.clone(),
                    });
                tracing::info!("Display capture started");
                if sender.send(ret).is_err() {
                    tracing::error!("Failed to send DisplayUpdates");
                }
            }
            Job::CaptureStop(index) => {
                tracing::info!("Stopping display capture");
                self.stream
                    .remove_output_handler(index.to_raw(), SCStreamOutputType::Screen);
            }
        }
    }
}
