use aviutl2::{
    anyhow,
    filter::{FilterConfigItemSliceExt, FilterConfigItems},
    log,
};

#[aviutl2::plugin(GenericPlugin)]
struct SvgAux2 {
    filter: aviutl2::generic::SubPlugin<SvgFilter>,
}

impl aviutl2::generic::GenericPlugin for SvgAux2 {
    fn new(info: aviutl2::AviUtl2Info) -> aviutl2::AnyResult<Self> {
        Ok(Self {
            filter: aviutl2::generic::SubPlugin::new_filter_plugin(&info)?,
        })
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        registry.register_filter_plugin(&self.filter);
    }

    fn on_clear_cache(&mut self, _edit_section: &aviutl2::generic::EditSection) {
        log::info!("Clearing SVG caches");
        SVG_CACHES.clear();
    }
}

#[derive(Clone, Default, educe::Educe)]
#[educe(Debug, PartialEq)]
struct SvgCacheEntry {
    path: std::path::PathBuf,
    color: (u8, u8, u8),

    width: u32,
    height: u32,
    maintain_aspect_ratio: bool,
    clipping: (u32, u32, u32, u32),
    #[educe(Debug(ignore), PartialEq(ignore))]
    buffer: Vec<u8>,
}

static SVG_CACHES: std::sync::LazyLock<dashmap::DashMap<i64, SvgCacheEntry>> =
    std::sync::LazyLock::new(dashmap::DashMap::new);

#[aviutl2::plugin(FilterPlugin)]
struct SvgFilter {}

#[aviutl2::filter::filter_config_items]
struct SvgConfig {
    #[track(name = "幅", range=1..=8192, default = 100, step = 1.0)]
    width: u32,
    #[track(name = "高さ", range=1..=8192, default = 100, step = 1.0)]
    height: u32,
    #[check(name = "アスペクト比の維持", default = true)]
    maintain_aspect_ratio: bool,
    #[file(name = "ファイル", filters = { "SVG" => ["svg"] })]
    svg_file: Option<std::path::PathBuf>,
    #[color(name = "色", default = 0xffffff)]
    color: aviutl2::filter::FilterConfigColorValue,
    #[group(name = "クリッピング", opened = false)]
    clipping: group! {
        #[track(name = "左", range = 0..=8192, default = 0, step = 1.0)]
        clip_left: u32,
        #[track(name = "上", range = 0..=8192, default = 0, step = 1.0)]
        clip_top: u32,
        #[track(name = "右", range = 0..=8192, default = 0, step = 1.0)]
        clip_right: u32,
        #[track(name = "下", range = 0..=8192, default = 0, step = 1.0)]
        clip_bottom: u32,
    },
}

impl aviutl2::filter::FilterPlugin for SvgFilter {
    fn new(_info: aviutl2::AviUtl2Info) -> aviutl2::AnyResult<Self> {
        aviutl2::logger::LogBuilder::new()
            .filter_level(if cfg!(debug_assertions) {
                aviutl2::logger::LevelFilter::Debug
            } else {
                aviutl2::logger::LevelFilter::Info
            })
            .init();
        Ok(Self {})
    }

    fn plugin_info(&self) -> aviutl2::filter::FilterPluginTable {
        aviutl2::filter::FilterPluginTable {
            name: "SVG".into(),
            label: None,
            flags: aviutl2::bitflag!(aviutl2::filter::FilterPluginFlags {
                video: true,
                as_object: true
            }),
            information: format!(
                "SVG Object, powered by resvg, written in Rust / v{version} / https://github.com/sevenc-nanashi/svg.aux2",
                version = env!("CARGO_PKG_VERSION")
            ),
            config_items: SvgConfig::to_config_items(),
        }
    }

    fn proc_video(
        &self,
        config: &[aviutl2::filter::FilterConfigItem],
        video: &mut aviutl2::filter::FilterProcVideo,
    ) -> aviutl2::AnyResult<()> {
        let config = config.to_struct::<SvgConfig>();
        let Some(svg_path) = &config.svg_file else {
            return Ok(());
        };
        let color = config.color.to_rgb();

        let mut cache_entry = SVG_CACHES.entry(video.object.effect_id).or_default();
        let cache_key = SvgCacheEntry {
            path: svg_path.clone(),
            color,
            width: config.width,
            height: config.height,
            maintain_aspect_ratio: config.maintain_aspect_ratio,
            clipping: (
                config.clip_left,
                config.clip_top,
                config.clip_right,
                config.clip_bottom,
            ),
            buffer: Vec::new(),
        };
        if *cache_entry.value() != cache_key {
            log::info!(
                "Rendering SVG file '{}' with color rgb({},{},{}) at size {}x{}",
                svg_path.display(),
                color.0,
                color.1,
                color.2,
                config.width,
                config.height
            );
            let svg_data = std::fs::read_to_string(svg_path).map_err(|e| {
                anyhow::anyhow!("Failed to read SVG file '{}': {}", svg_path.display(), e)
            })?;
            let opt = resvg::usvg::Options {
                style_sheet: Some(format!(
                    "* {{ color: rgb({},{},{}); }}",
                    color.0, color.1, color.2
                )),
                ..Default::default()
            };
            let rtree = resvg::usvg::Tree::from_str(&svg_data, &opt).map_err(|e| {
                anyhow::anyhow!(
                    "Failed to create SVG tree from file '{}': {}",
                    svg_path.display(),
                    e
                )
            })?;
            let (clipped_width, clipped_height) = {
                let size = rtree.size();
                let clipped_width =
                    (size.width() as u32).saturating_sub(config.clip_left + config.clip_right);
                let clipped_height =
                    (size.height() as u32).saturating_sub(config.clip_top + config.clip_bottom);
                (clipped_width, clipped_height)
            };
            let (scale_x, scale_y) = if config.maintain_aspect_ratio {
                let scale_x = config.width as f32 / clipped_width as f32;
                let scale_y = config.height as f32 / clipped_height as f32;
                let scale = scale_x.min(scale_y);
                (scale, scale)
            } else {
                (
                    config.width as f32 / clipped_width as f32,
                    config.height as f32 / clipped_height as f32,
                )
            };
            log::debug!(
                "Clipped SVG size: {}x{}, scale: {}x{}",
                clipped_width,
                clipped_height,
                scale_x,
                scale_y
            );
            let canvas_width = (clipped_width as f32 * scale_x).ceil() as u32;
            let canvas_height = (clipped_height as f32 * scale_y).ceil() as u32;
            if canvas_width == 0 || canvas_height == 0 {
                return Err(anyhow::anyhow!(
                    "Resulting SVG size is zero ({}x{})",
                    canvas_width,
                    canvas_height
                ));
            }
            log::debug!("Canvas size: {}x{}", canvas_width, canvas_height);
            let mut buf =
                resvg::tiny_skia::Pixmap::new(canvas_width, canvas_height).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Failed to create pixmap with size {}x{}",
                        canvas_width,
                        canvas_height
                    )
                })?;
            let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y)
                .pre_translate(-(config.clip_left as f32), -(config.clip_top as f32));
            resvg::render(&rtree, transform, &mut buf.as_mut());
            *cache_entry.value_mut() =
                if config.width == buf.width() && config.height == buf.height() {
                    SvgCacheEntry {
                        buffer: buf.data().to_vec(),
                        ..cache_key
                    }
                } else {
                    let mut final_buf = resvg::tiny_skia::Pixmap::new(config.width, config.height)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "Failed to create final pixmap with size {}x{}",
                                config.width,
                                config.height
                            )
                        })?;
                    final_buf.fill(resvg::tiny_skia::Color::from_rgba8(0, 0, 0, 0));
                    let left = ((config.width as i32 - buf.width() as i32) / 2).max(0) as u32;
                    let top = ((config.height as i32 - buf.height() as i32) / 2).max(0) as u32;
                    final_buf.draw_pixmap(
                        left as i32,
                        top as i32,
                        buf.as_ref(),
                        &resvg::tiny_skia::PixmapPaint::default(),
                        Default::default(),
                        None,
                    );
                    SvgCacheEntry {
                        buffer: final_buf.data().to_vec(),
                        ..cache_key
                    }
                };
        }

        let cache_entry = cache_entry.value();
        video.set_image_data(&cache_entry.buffer, cache_entry.width, cache_entry.height);
        Ok(())
    }
}

aviutl2::register_generic_plugin!(SvgAux2);
