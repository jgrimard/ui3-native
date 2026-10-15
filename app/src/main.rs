//! UI3 Native — a Rust port of Blue Iris UI3 for Windows with direct H.264/H.265 hardware-decoded playback.

#![windows_subsystem = "windows"]

mod app;
mod creds;
mod events;
mod mqtt;
mod gpu;
mod player;
mod runtime;
mod scrub;
mod settings;
mod settings_defs;
mod icons;
mod toasts;
mod views;

fn main() -> eframe::Result {
    let log_dir = settings::config_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let file = std::fs::OpenOptions::new().create(true).append(true).open(log_dir.join("ui3-native.log")).ok();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into());
    match file {
        Some(f) => tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(std::sync::Mutex::new(f)).init(),
        None => tracing_subscriber::fmt().with_env_filter(filter).init(),
    }
    tracing::info!("UI3 Native {} starting; FFmpeg avcodec {}", env!("CARGO_PKG_VERSION"), bi_media::ffmpeg_version());
    bi_media::quiet_ffmpeg_logs();
    runtime::init();

    let mut wgpu_options = egui_wgpu::WgpuConfiguration::default();
    if let egui_wgpu::WgpuSetup::CreateNew(c) = &mut wgpu_options.wgpu_setup {
        c.instance_descriptor.backends = egui_wgpu::wgpu::Backends::DX12;
        c.power_preference = egui_wgpu::wgpu::PowerPreference::HighPerformance;
        // NV12 textures let decoded frames be sampled straight from the decoder's D3D11 textures (zero-copy).
        c.device_descriptor = std::sync::Arc::new(|adapter| {
            let wanted = egui_wgpu::wgpu::Features::TEXTURE_FORMAT_NV12;
            egui_wgpu::wgpu::DeviceDescriptor {
                label: Some("ui3 device"),
                required_features: adapter.features() & wanted,
                required_limits: egui_wgpu::wgpu::Limits { max_texture_dimension_2d: 8192, ..Default::default() },
                ..Default::default()
            }
        });
    }
    // Low latency presentation: video frames arrive asynchronously.
    wgpu_options.surface = egui_wgpu::SurfaceConfig::LOW_LATENCY;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("UI3 Native").with_inner_size([1400.0, 860.0]).with_min_inner_size([640.0, 400.0]).with_icon(app::icon()),
        renderer: eframe::Renderer::Wgpu,
        wgpu_options,
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native("UI3 Native", options, Box::new(|cc| Ok(Box::new(app::UiApp::new(cc)))))
}
