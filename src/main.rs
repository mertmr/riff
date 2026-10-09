mod app;
mod model;
mod player;
#[cfg(feature = "ui-qa")]
mod qa;
mod radio;
mod search;
mod storage;
mod ui;

fn main() -> eframe::Result {
    #[cfg(feature = "ui-qa")]
    qa::start_timer();
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/icons/riff-256.png"))
        .expect("bundled Riff icon must be a valid PNG");
    let wgpu_options = {
        let mut gpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
        let descriptor = gpu_setup.device_descriptor;
        gpu_setup.device_descriptor = std::sync::Arc::new(move |adapter| {
            let mut descriptor = descriptor(adapter);
            // Honored by allocators such as Vulkan's; Metal currently ignores it.
            descriptor.memory_hints = eframe::wgpu::MemoryHints::MemoryUsage;
            descriptor
        });
        eframe::WgpuConfiguration {
            surface: eframe::SurfaceConfig::LOW_LATENCY,
            wgpu_setup: gpu_setup.into(),
            ..Default::default()
        }
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Riff")
            .with_icon(icon)
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 600.0]),
        renderer: eframe::Renderer::Wgpu,
        // One queued frame is sufficient for this 2D UI and avoids another
        // full Retina surface being kept in flight.
        wgpu_options,
        ..Default::default()
    };

    #[cfg(feature = "ui-qa")]
    let options = if std::env::var_os("RIFF_CAPTURE").is_some() {
        eframe::NativeOptions {
            viewport: options
                .viewport
                .with_inner_size(qa::window_size())
                .with_min_inner_size(qa::window_size())
                .with_max_inner_size(qa::window_size())
                .with_window_level(eframe::egui::WindowLevel::AlwaysOnTop)
                .with_clamp_size_to_monitor_size(false),
            ..options
        }
    } else {
        options
    };

    eframe::run_native(
        "Riff",
        options,
        Box::new(|cc| Ok(Box::new(app::MusicApp::new(cc)))),
    )
}
