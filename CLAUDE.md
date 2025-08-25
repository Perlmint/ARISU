## Project Overview
Arisu is a macOS RDP (Remote Desktop Protocol) server application that provides:
- Screen capture and sharing via ScreenCaptureKit
- Audio capture and streaming
- User authentication with configurable credentials
- TLS/Hybrid security support
- GUI settings management via status bar menu

## Architecture
- **Main Application**: GUI-driven app with status bar interface
- **Server Controller**: Event-driven server management with Watch channels for status updates
- **Screen Capture**: Uses ScreenCaptureKit for display and audio capture with graceful shutdown support
- **Configuration**: TOML-based user settings stored in Application Support directory
- **Logging**: Multi-output tracing (stdout, file, macOS Console) for debugging

## Platform Notes
- This application only targeted for macOS
- Uses objc2 and objc2-app-kit for native macOS GUI
- Requires ScreenCaptureKit framework (macOS 12.3+)

## Development Guidelines
- Make unsafe block as small as possible. Make SAFETY note for every unsafe items.
- Always use LocalSet for !Send futures in tokio runtime
- Implement graceful shutdown for all async components
- Use Watch channels for reactive status updates instead of polling

## Build Process
- Bundle: `cargo bundle`
- **Code signing highly recommended**: Without code signing, each build invalidates previously granted permissions (screen recording, microphone, etc.) as macOS treats each unsigned build as a different application. The macOS Settings UI doesn't clearly show this permission invalidation, making debugging difficult.
- Example: `codesign --force --deep --sign "Your Developer Certificate" target/debug/bundle/osx/ARISU.app`

## Current Implementation Status
- ✅ GUI settings dialog with modal behavior
- ✅ Server start/stop control with real-time status updates
- ✅ Configuration persistence (TOML format)
- ✅ Event-driven server architecture
- ✅ Screen capture with graceful shutdown
- ✅ Multi-output logging (debug file, console, stdout)
- ✅ Panic handling for ServerManager with application termination
- ✅ Audio handler management with proper cleanup to prevent SCStream errors when no client connected