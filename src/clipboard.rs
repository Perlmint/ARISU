use std::sync::{Arc, RwLock};

use ironrdp::{
    cliprdr::{backend::{ClipboardMessage, CliprdrBackend, CliprdrBackendFactory}, pdu::{ClipboardFormat, ClipboardFormatId, ClipboardFormatName, ClipboardGeneralCapabilityFlags}},
    core::AsAny,
    server::{CliprdrServerFactory, ServerEvent, ServerEventSender},
};
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSFileManager, NSString, NSTemporaryDirectory};
use tokio::sync::{mpsc, oneshot};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug)]
enum ClipboardJob {
    GetFormatList {
        response: oneshot::Sender<Vec<(u32, String)>>,
    },
    GetFormatData {
        format_id: u32,
        response: oneshot::Sender<Option<Vec<u8>>>,
    },
    SetClipboardData {
        data: Vec<u8>,
        format_id: u32,
        response: oneshot::Sender<bool>,
    },
}

pub struct ClipboardBackend {
    job_sender: mpsc::UnboundedSender<ClipboardJob>,
    _worker_handle: std::thread::JoinHandle<()>,
    temp_dir: String,
    server_event_sender: Option<UnboundedSender<ServerEvent>>,
}

impl std::fmt::Debug for ClipboardBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardBackend")
            .field("job_sender", &"mpsc::UnboundedSender<ClipboardJob>")
            .field("_worker_handle", &"std::thread::JoinHandle<()>")
            .finish()
    }
}

impl ClipboardBackend {
    pub fn new() -> Self {
        let (job_sender, job_receiver) = mpsc::unbounded_channel();
        
        // Create temporary directory using macOS APIs
        let temp_dir = Self::create_temp_directory();
        
        // Spawn clipboard processing thread
        let worker_handle = std::thread::spawn(move || {
            Self::clipboard_worker(job_receiver);
        });
        
        Self { 
            job_sender,
            _worker_handle: worker_handle,
            temp_dir,
            server_event_sender: None,
        }
    }
    
    fn create_temp_directory() -> String {
        // SAFETY: NSTemporaryDirectory and NSFileManager are safe to call from any thread
        let temp_base = unsafe { NSTemporaryDirectory() };
        let temp_base_str = temp_base.to_string();
        
        // Create a unique subdirectory for clipboard operations
        let unique_dir = format!("{}/arisu_clipboard_{}", temp_base_str, std::process::id());
        let unique_dir_ns = NSString::from_str(&unique_dir);
        
        let file_manager = unsafe { NSFileManager::defaultManager() };
        
        // Create the directory
        let success = unsafe {
            file_manager.createDirectoryAtPath_withIntermediateDirectories_attributes_error(
                &unique_dir_ns,
                true,
                None,
            )
        };
        
        if success.is_ok() {
            tracing::info!("Created temporary directory: {}", unique_dir);
            unique_dir
        } else {
            tracing::warn!("Failed to create temporary directory, falling back to base temp dir");
            temp_base_str
        }
    }
    
    fn clipboard_worker(mut job_receiver: mpsc::UnboundedReceiver<ClipboardJob>) {
        // SAFETY: NSPasteboard must be accessed from the main thread or a thread that has
        // been configured for Cocoa. This worker thread will handle all NSPasteboard operations.
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        
        while let Some(job) = job_receiver.blocking_recv() {
            match job {
                ClipboardJob::GetFormatList { response } => {
                    // Get available formats from NSPasteboard
                    let mut formats = Vec::new();
                    
                    // Check if string data is available
                    let has_string = unsafe { 
                        pasteboard.stringForType(&NSPasteboardTypeString).is_some() 
                    };
                    
                    if has_string {
                        formats.push((1, "CF_TEXT".to_string()));
                        formats.push((13, "CF_UNICODETEXT".to_string()));
                    }
                    
                    // TODO: Add more format types (images, files, etc.)
                    
                    let _ = response.send(formats);
                }
                ClipboardJob::GetFormatData { format_id, response } => {
                    // Get clipboard data for specific format
                    let data = match format_id {
                        1 | 13 => {
                            // CF_TEXT or CF_UNICODETEXT
                            unsafe {
                                pasteboard.stringForType(&NSPasteboardTypeString)
                                    .map(|s| s.to_string().into_bytes())
                            }
                        }
                        _ => {
                            tracing::warn!("Unsupported format ID: {}", format_id);
                            None
                        }
                    };
                    
                    let _ = response.send(data);
                }
                ClipboardJob::SetClipboardData { data, format_id, response } => {
                    // Set clipboard data with specific format
                    let success = match format_id {
                        1 | 13 => {
                            // CF_TEXT or CF_UNICODETEXT
                            if let Ok(text) = String::from_utf8(data) {
                                let ns_string = NSString::from_str(&text);
                                unsafe {
                                    pasteboard.clearContents();
                                    pasteboard.setString_forType(&ns_string, &NSPasteboardTypeString)
                                }
                            } else {
                                false
                            }
                        }
                        _ => {
                            tracing::warn!("Unsupported format ID for setting: {}", format_id);
                            false
                        }
                    };
                    
                    let _ = response.send(success);
                }
            }
        }
    }
}

impl AsAny for ClipboardBackend {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

impl CliprdrBackend for ClipboardBackend {
    fn temporary_directory(&self) -> &str {
        &self.temp_dir
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        tracing::info!("Clipboard backend ready");
    }

    fn on_request_format_list(&mut self) {
        tracing::info!("Clipboard format list requested");
        
        if let Some(sender) = &self.server_event_sender {
            let (response_tx, response_rx) = oneshot::channel();
            
            // Send job to get format list
            if self.job_sender.send(ClipboardJob::GetFormatList { response: response_tx }).is_ok() {
                // Wait for response from worker thread
                if let Ok(formats) = response_rx.blocking_recv() {
                    // Convert to ClipboardFormat and send format list
                    let clipboard_formats: Vec<ClipboardFormat> = formats.into_iter().map(|(id, name)| {
                        ClipboardFormat {
                            id: ClipboardFormatId(id),
                            name: Some(ClipboardFormatName::new(name)),
                        }
                    }).collect();
                    
                    let format_list_event = ServerEvent::Clipboard(ClipboardMessage::FormatList(clipboard_formats));
                    if let Err(e) = sender.send(format_list_event) {
                        tracing::error!("Failed to send format list: {:?}", e);
                    }
                }
            }
        }
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        _capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        tracing::info!("Clipboard capabilities negotiated: {:?}", _capabilities);
    }

    fn on_remote_copy(&mut self, available_formats: &[ironrdp::cliprdr::pdu::ClipboardFormat]) {
        tracing::info!("Remote copy operation with {} formats", available_formats.len());
        
        // Log available formats for debugging
        for format in available_formats {
            tracing::debug!("Available format: ID={:?}, Name={:?}", 
                format.id, 
                format.name.as_ref().map(|n| n.value())
            );
        }
        
        // Store the available formats (in a real implementation, you might want to 
        // store these in the backend for later use)
        tracing::info!("Remote clipboard formats processed and ready for paste operations");
    }

    fn on_format_data_request(&mut self, format_req: ironrdp::cliprdr::pdu::FormatDataRequest) {
        tracing::info!("Format data requested for format: {:?}", format_req.format);
        
        if let Some(sender) = &self.server_event_sender {
            let (response_tx, response_rx) = oneshot::channel();
            
            // Send job to get format data
            let job = ClipboardJob::GetFormatData {
                format_id: format_req.format.0,
                response: response_tx,
            };
            
            if self.job_sender.send(job).is_ok() {
                // Wait for response from worker thread
                if let Ok(data) = response_rx.blocking_recv() {
                    let format_data_event = if let Some(clipboard_data) = data {
                        ServerEvent::Clipboard(ClipboardMessage::FormatDataResponse(clipboard_data))
                    } else {
                        // Send empty response if no data
                        ServerEvent::Clipboard(ClipboardMessage::FormatDataResponse(vec![]))
                    };
                    
                    if let Err(e) = sender.send(format_data_event) {
                        tracing::error!("Failed to send format data response: {:?}", e);
                    }
                }
            }
        }
    }

    fn on_format_data_response(&mut self, response: ironrdp::cliprdr::pdu::FormatDataResponse<'_>) {
        tracing::info!("Format data response received");
        
        let (response_tx, response_rx) = oneshot::channel();
        
        // Send job to set clipboard data
        let job = ClipboardJob::SetClipboardData {
            data: response.data().to_vec(),
            format_id: 1, // Default to text format for now
            response: response_tx,
        };
        
        if self.job_sender.send(job).is_ok() {
            // Wait for response from worker thread
            if let Ok(success) = response_rx.blocking_recv() {
                if success {
                    tracing::info!("Successfully set clipboard data");
                } else {
                    tracing::error!("Failed to set clipboard data");
                }
            }
        }
    }

    fn on_file_contents_request(&mut self, _request: ironrdp::cliprdr::pdu::FileContentsRequest) {
        tracing::info!("File contents request: {:?}", _request.stream_id);
        // TODO: Handle file contents request
    }

    fn on_file_contents_response(
        &mut self,
        _response: ironrdp::cliprdr::pdu::FileContentsResponse<'_>,
    ) {
        tracing::info!("File contents response received");
        // TODO: Handle file contents response
    }

    fn on_lock(&mut self, _data_id: ironrdp::cliprdr::pdu::LockDataId) {
        tracing::info!("Clipboard lock requested: {:?}", _data_id);
        // TODO: Lock clipboard data
    }

    fn on_unlock(&mut self, _data_id: ironrdp::cliprdr::pdu::LockDataId) {
        tracing::info!("Clipboard unlock requested: {:?}", _data_id);
        // TODO: Unlock clipboard data
    }
}

pub struct ClipboardServerFactory {
    inner: Arc<RwLock<Option<UnboundedSender<ServerEvent>>>>,
}

impl ClipboardServerFactory {
    pub fn new() -> Self {
        Self {
            inner: Default::default(),
        }
    }
}

impl CliprdrBackendFactory for ClipboardServerFactory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        let mut backend = ClipboardBackend::new();
        // Pass the server event sender to the backend
        if let Ok(inner) = self.inner.read() {
            if let Some(sender) = inner.as_ref() {
                backend.server_event_sender = Some(sender.clone());
            }
        }
        Box::new(backend)
    }
}

impl ServerEventSender for ClipboardServerFactory {
    fn set_sender(&mut self, sender: UnboundedSender<ServerEvent>) {
        let mut inner = self.inner.write().expect("Failed to retreive write lock");
        *inner = Some(sender);
    }
}

impl CliprdrServerFactory for ClipboardServerFactory {}
