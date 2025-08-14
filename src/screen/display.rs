use anyhow::{anyhow, Context as _};
use bytes::Bytes;
use dispatch2::DispatchQueue;
use ironrdp::server::{
    BitmapUpdate, DesktopSize, DisplayUpdate, RdpServerDisplay, RdpServerDisplayUpdates,
};
use objc2::{
    define_class, msg_send, rc::Retained, runtime::ProtocolObject, AnyThread as _,
    DefinedClass as _,
};
use objc2_core_foundation::{CFArray, CFDictionary, CFNumber, CFRetained, CFString, CFType};
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddress, CVPixelBufferGetBaseAddressOfPlane,
    CVPixelBufferGetBytesPerRow, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetPlaneCount, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags,
};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_screen_capture_kit::{
    SCFrameStatus, SCStream, SCStreamFrameInfoDirtyRects, SCStreamFrameInfoStatus, SCStreamOutput,
    SCStreamOutputType,
};
use std::{
    cell::RefCell,
    mem::transmute,
    num::NonZeroU16,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tokio::sync::{mpsc, oneshot, watch, Notify};

use crate::{counter::IntervalCounter, screen::ScreenJob};

use super::ScreenSize;

const BYTES_PER_PIXEL: usize = 4; // BGRA format

fn dict_to_rect(
    rect: &CFDictionary<CFString, CFNumber>,
) -> anyhow::Result<(usize, usize, usize, usize)> {
    let x = unsafe { rect.get_unchecked(&CFString::from_static_str("X")) }
        .ok_or(anyhow!("X is not found"))?
        .as_f64()
        .ok_or(anyhow!("Failed to convert X to f64"))? as usize;
    let y = unsafe { rect.get_unchecked(&CFString::from_static_str("Y")) }
        .ok_or(anyhow!("Y is not found"))?
        .as_f64()
        .ok_or(anyhow!("Failed to convert Y to f64"))? as usize;
    let width = unsafe { rect.get_unchecked(&CFString::from_static_str("Width")) }
        .ok_or(anyhow!("Width is not found"))?
        .as_f64()
        .ok_or(anyhow!("Failed to convert Width to f64"))? as usize;
    let height = unsafe { rect.get_unchecked(&CFString::from_static_str("Height")) }
        .ok_or(anyhow!("Height is not found"))?
        .as_f64()
        .ok_or(anyhow!("Failed to convert Height to f64"))? as usize;
    Ok((x, y, width, height))
}

pub(super) enum Job {
    GetSize(oneshot::Sender<(u16, u16)>),
    SetSize(u16, u16),
    CaptureStart(oneshot::Sender<anyhow::Result<DisplayUpdates>>),
    CaptureStop,
}

#[derive(Debug, Clone)]
struct CapturedData {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
    stride: usize,
    data: Vec<u8>,
}

pub(super) struct DisplayUpdates {
    display_sender: mpsc::Sender<ScreenJob>,
    capture_receiver: triple_buffer::Output<CapturedData>,
    #[allow(dead_code)]
    display_size: watch::Receiver<ScreenSize>,
    update_notification: Arc<Notify>,
    send_counter: IntervalCounter,
}

impl Drop for DisplayUpdates {
    fn drop(&mut self) {
        let _ = self
            .display_sender
            .try_send(ScreenJob::Display(Job::CaptureStop));
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
            stride,
            data: buffer,
        } = self.capture_receiver.peek_output_buffer();
        self.send_counter.update();
        let start_offset = *y as usize * *stride + *x as usize * BYTES_PER_PIXEL;
        let end_offset =
            start_offset + (*height as usize - 1) * *stride + *width as usize * BYTES_PER_PIXEL;
        tracing::info!(
            "x: {}, y: {}, width: {}, height: {}, start_offset: {}, end_offset: {}",
            *x,
            *y,
            *width,
            *height,
            start_offset,
            end_offset
        );
        Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x: *x,
            y: *y,
            width: unsafe { NonZeroU16::new_unchecked(*width) },
            height: unsafe { NonZeroU16::new_unchecked(*height) },
            format: ironrdp::server::PixelFormat::BgrA32,
            data: Bytes::from_static(unsafe {
                &*(&buffer[start_offset..end_offset] as *const [u8])
            }),
            stride: *stride,
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
        tracing::info!("display size: {width} x {height}");
        DesktopSize { width, height }
    }

    async fn updates(&mut self) -> anyhow::Result<Box<dyn RdpServerDisplayUpdates>> {
        let (sender, receiver) = oneshot::channel();
        self.job_sender
            .send(ScreenJob::Display(Job::CaptureStart(sender)))
            .await
            .context("Failed to send capture start job to display handler")?;

        tracing::info!("updates(): CaptureStart job sent, waiting for response");
        let received = receiver
            .await
            .context("Failed to receive response from display capture")?
            .context("Display capture initialization failed")?;

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
    let plane_count = unsafe { CVPixelBufferGetPlaneCount(input) };
    let lock_result =
        unsafe { CVPixelBufferLockBaseAddress(input, CVPixelBufferLockFlags::ReadOnly) };
    if lock_result != 0 {
        tracing::error!("Failed to lock pixel buffer base address - {lock_result}");
        return false;
    };

    let (base_address, bytes_per_row) = if plane_count == 0 {
        unsafe {
            (
                CVPixelBufferGetBaseAddress(input),
                CVPixelBufferGetBytesPerRow(input),
            )
        }
    } else {
        unsafe {
            (
                CVPixelBufferGetBaseAddressOfPlane(input, 0),
                CVPixelBufferGetBytesPerRowOfPlane(input, 0),
            )
        }
    };
    let base_address = base_address as *const u8;
    let data_size = (height - 1) * bytes_per_row + width * BYTES_PER_PIXEL;
    let out_addr = unsafe {
        output
            .data
            .as_mut_ptr()
            .add(y * (bytes_per_row) + x * BYTES_PER_PIXEL)
    };
    let src_addr = unsafe { base_address.add(y * (bytes_per_row) + x * BYTES_PER_PIXEL) };
    unsafe {
        std::ptr::copy_nonoverlapping(src_addr, out_addr, data_size);
    }

    output.x = x as _;
    output.y = y as _;
    output.width = width as _;
    output.height = height as _;
    output.stride = bytes_per_row;

    true
}

struct DisplayCaptureDelegateIvars {
    sender: RefCell<triple_buffer::Input<CapturedData>>,
    update_notifier: Arc<Notify>,
    capture_counter: RefCell<IntervalCounter>,
    is_first_submission: AtomicBool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[ivars = DisplayCaptureDelegateIvars]
    struct DisplayCaptureDelegate;

    unsafe impl NSObjectProtocol for DisplayCaptureDelegate {}

    unsafe impl SCStreamOutput for DisplayCaptureDelegate {
        #[allow(non_snake_case)]
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_didOutputSampleBuffer_ofType(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            of_type: SCStreamOutputType,
        ) {
            if let Err(e) = Self::handle_stream(self.ivars(), _stream, sample_buffer, of_type) {
                tracing::error!("Error in handle_stream: {e:?}");
            }
        }
    }
);

impl DisplayCaptureDelegate {
    pub(crate) fn new(
        sender: triple_buffer::Input<CapturedData>,
        update_notifier: Arc<Notify>,
        capture_counter: IntervalCounter,
    ) -> Retained<Self> {
        let this = DisplayCaptureDelegate::alloc();
        let this = this.set_ivars(DisplayCaptureDelegateIvars {
            sender: RefCell::new(sender),
            update_notifier,
            capture_counter: RefCell::new(capture_counter),
            is_first_submission: AtomicBool::new(true),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn handle_stream(
        ivars: &DisplayCaptureDelegateIvars,
        _stream: &SCStream,
        sample_buffer: &CMSampleBuffer,
        of_type: SCStreamOutputType,
    ) -> anyhow::Result<()> {
        if of_type != SCStreamOutputType::Screen {
            return Err(anyhow!("non-screen received"));
        }

        let Some(attachments) = (unsafe { sample_buffer.sample_attachments_array(false) }) else {
            return Err(anyhow!("No attachments found in sample buffer"));
        };
        let attachments = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(attachments) };
        let Some(attachments) = attachments.get(0) else {
            return Err(anyhow!("No attachments found in sample buffer"));
        };
        let Ok(attachments) = attachments.downcast::<CFDictionary>() else {
            return Err(anyhow!("Failed to downcast sample buffer attachments"));
        };
        let attachments =
            unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFType>>(attachments) };
        let Some(status) = attachments.get(unsafe { transmute(SCStreamFrameInfoStatus) }) else {
            return Err(anyhow!(
                "Failed to get frame info status from sample buffer"
            ));
        };
        let status = status.downcast::<CFNumber>();
        let Ok(status) = status else {
            return Err(anyhow!("Failed to downcast frame info status"));
        };
        let Some(status) = status.as_i64() else {
            return Err(anyhow!("Failed to convert frame info status to i64"));
        };
        let status = SCFrameStatus(status as _);
        if status != SCFrameStatus::Complete {
            tracing::trace!("not completed");
            return Ok(());
        }

        let Some(dirty_rects) = attachments.get(unsafe { transmute(SCStreamFrameInfoDirtyRects) })
        else {
            return Err(anyhow!(
                "Failed to get frame info dirty rects from sample buffer"
            ));
        };
        let Ok(dirty_rects) = dirty_rects.downcast::<CFArray>() else {
            return Err(anyhow!("Failed to downcast dirty rects"));
        };
        let dirty_rects = unsafe { CFRetained::cast_unchecked::<CFArray<CFType>>(dirty_rects) };

        let Some(pixel_buffer) = (unsafe { sample_buffer.image_buffer() }) else {
            return Err(anyhow!("Failed to get image buffer from sample buffer"));
        };
        let pixel_buffer = pixel_buffer.as_ref();
        let Ok((mut x, mut y, max_x, max_y)) = dirty_rects.iter().try_fold(
            (usize::MAX, usize::MAX, 0, 0),
            |(min_x, min_y, max_x, max_y), rect| {
                let Ok(rect) = rect.downcast::<CFDictionary>() else {
                    return Err(anyhow!("Failed to downcast dirty rect"));
                };
                let rect =
                    unsafe { CFRetained::cast_unchecked::<CFDictionary<CFString, CFNumber>>(rect) };
                let (x, y, width, height) =
                    dict_to_rect(&rect).context("Failed to convert dirty rect to coordinates")?;

                Ok((
                    min_x.min(x),
                    min_y.min(y),
                    max_x.max(x + width),
                    max_y.max(y + height),
                ))
            },
        ) else {
            return Err(anyhow!("Failed to merge dirty rects"));
        };
        let mut width = max_x - x;
        let mut height = max_y - y;
        let is_first = ivars.is_first_submission.swap(false, Ordering::Relaxed);

        if width == 0 || height == 0 {
            if is_first {
                x = 0;
                y = 0;
                width = unsafe { CVPixelBufferGetWidth(pixel_buffer) } as usize;
                height = unsafe { CVPixelBufferGetHeight(pixel_buffer) } as usize;
                tracing::info!("First submission, sending full buffer: {width} x {height}");
            } else {
                tracing::trace!("No dirty rects found on non-first submission, skipping");
                return Ok(());
            }
        } else {
            tracing::info!("Dirty rects found: x: {x}, y: {y}, width: {width}, height: {height}");
        }

        let mut input_buffer = ivars.sender.borrow_mut();
        {
            let input_buffer = input_buffer.input_buffer_mut();
            if !convert_buffer(x, y, width, height, &pixel_buffer, input_buffer) {
                return Err(anyhow!("Failed to convert buffer"));
            };
        }
        input_buffer.publish();
        ivars.update_notifier.notify_waiters();
        ivars.capture_counter.borrow_mut().update();
        Ok(())
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
                // unsafe {
                //     self.capture_config.setWidth(width as _);
                //     self.capture_config.setHeight(height as _);
                // }
                // unsafe {
                //     self.stream
                //         .updateConfiguration_completionHandler(&self.capture_config, None)
                // };
                // // use objc2_core_graphics::{CGGetActiveDisplayList, CGDisplayCopyDisplayMode, CGDisplayMode, CGDirectDisplayID};
                // // let mut active_displays = std::mem::MaybeUninit::<[CGDirectDisplayID; 1]>::uninit();
                // // let mut display_count = std::mem::MaybeUninit::<u32>::uninit();
                // // unsafe { CGGetActiveDisplayList(1, &raw mut (&mut *active_displays.as_mut_ptr())[0], display_count.as_mut_ptr()) };
                // // let active_displays = unsafe { active_displays.assume_init() };
                // // let display_count = unsafe { display_count.assume_init() };
                // // if display_count == 0 {
                // //     panic!("No active displays found");
                // // }
                // // let display = active_displays[0];
                // // let display_mode = unsafe { CGDisplayCopyDisplayMode(display) }.unwrap();
                // self.display_size.send_if_modified(|screen_size| {
                //     if screen_size.client != (width, height) {
                //         tracing::info!("Client display size changed: {} x {}", width, height);
                //         screen_size.client = (width, height);
                //         true
                //     } else {
                //         false
                //     }
                // });
            }
            Job::CaptureStart(sender) => {
                let screen_size = *self.display_size.borrow();
                let buffer_size = BYTES_PER_PIXEL
                    * (screen_size.server.0 as usize)
                    * (screen_size.server.1 as usize);
                let (capture_sender, capture_receiver) =
                    triple_buffer::triple_buffer(&CapturedData {
                        data: vec![0u8; buffer_size],
                        width: screen_size.server.0 as _,
                        height: screen_size.server.1 as _,
                        x: 0,
                        y: 0,
                        stride: BYTES_PER_PIXEL * (screen_size.server.0 as usize),
                    });
                let update_notification = Arc::new(Notify::new());
                let delegate = DisplayCaptureDelegate::new(
                    capture_sender,
                    update_notification.clone(),
                    self.capture_counter.clone(),
                );
                let delegate = ProtocolObject::from_retained(delegate);
                let dispatch_queue = DispatchQueue::new("app.perlmint.arisu.display", None);
                let ret = unsafe {
                    self.stream.addStreamOutput_type_sampleHandlerQueue_error(
                        &delegate,
                        SCStreamOutputType::Screen,
                        Some(&dispatch_queue),
                    )
                }
                .map(|_| DisplayUpdates {
                    display_sender: self.job_sender.clone(),
                    update_notification,
                    capture_receiver,
                    display_size: self.display_size.subscribe(),
                    send_counter: self.send_counter.clone(),
                })
                .map_err(|e| anyhow!("Failed to add display output: {e:?}"));
                tracing::info!("Display capture started");
                if ret.is_ok() {
                    self.display_delegate = Some((delegate, dispatch_queue.into()));
                }
                if sender.send(ret).is_err() {
                    tracing::error!("Failed to send DisplayUpdates");
                }
            }
            Job::CaptureStop => {
                if let Some((output, _)) = self.display_delegate.take() {
                    tracing::info!("Removing display output");
                    if let Err(e) = unsafe {
                        self.stream
                            .removeStreamOutput_type_error(&output, SCStreamOutputType::Screen)
                    } {
                        tracing::error!("Failed to remove display output: {e:?}");
                    };
                } else {
                    tracing::warn!("No display delegate found to stop");
                }
            }
        }
    }
}
