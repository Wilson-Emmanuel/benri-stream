pub mod quality;

#[cfg(feature = "gstreamer")]
pub mod gstreamer;

#[cfg(feature = "gstreamer")]
mod hls_uploader;
