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
    clip_top: u32,
    clip_bottom: u32,
    clip_left: u32,
    clip_right: u32,
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
    #[group(name = "クリッピング", opened = true)]
    clipping: group! {
        #[track(name = "上", range=0..=8192, default = 0, step = 1.0)]
        clip_top: u32,
        #[track(name = "下", range=0..=8192, default = 0, step = 1.0)]
        clip_bottom: u32,
        #[track(name = "左", range=0..=8192, default = 0, step = 1.0)]
        clip_left: u32,
        #[track(name = "右", range=0..=8192, default = 0, step = 1.0)]
        clip_right: u32,
    },
}

impl aviutl2::filter::FilterPlugin for SvgFilter {
    fn new(_info: aviutl2::AviUtl2Info) -> aviutl2::AnyResult<Self> {
        aviutl2::logger::LogBuilder::new()
            .filter_level(aviutl2::logger::LevelFilter::Info)
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
            clip_top: config.clip_top,
            clip_bottom: config.clip_bottom,
            clip_left: config.clip_left,
            clip_right: config.clip_right,
            buffer: Vec::new(),
        };
        if *cache_entry.value() != cache_key {
            log::info!(
                "Rendering SVG file '{}' with color rgb({},{},{}) at size {}x{}, clipping (t:{}, b:{}, l:{}, r:{})",
                svg_path.display(),
                color.0,
                color.1,
                color.2,
                config.width,
                config.height,
                config.clip_top,
                config.clip_bottom,
                config.clip_left,
                config.clip_right
            );
            let svg_data = std::fs::read(svg_path).map_err(|e| {
                anyhow::anyhow!("Failed to read SVG file '{}': {}", svg_path.display(), e)
            })?;
            let mut buf =
                resvg::tiny_skia::Pixmap::new(config.width, config.height).ok_or_else(|| {
                    anyhow::anyhow!(
                        "Failed to create pixmap with size {}x{}",
                        config.width,
                        config.height
                    )
                })?;
            let opt = resvg::usvg::Options {
                style_sheet: Some(format!(
                    "* {{ color: rgb({},{},{}); }}",
                    color.0, color.1, color.2
                )),
                ..Default::default()
            };
            let rtree = resvg::usvg::Tree::from_data(&svg_data, &opt).map_err(|e| {
                anyhow::anyhow!("Failed to parse SVG file '{}': {}", svg_path.display(), e)
            })?;
            let svg_size = rtree.size();
            let target_width = config.width as f32;
            let target_height = config.height as f32;
            let scale_x = target_width / svg_size.width();
            let scale_y = target_height / svg_size.height();
            let (transform, scaled_width, scaled_height) = if config.maintain_aspect_ratio {
                let scale = scale_x.min(scale_y);
                let scaled_width = svg_size.width() * scale;
                let scaled_height = svg_size.height() * scale;
                let translate_x = (target_width - scaled_width) * 0.5;
                let translate_y = (target_height - scaled_height) * 0.5;
                (
                    resvg::tiny_skia::Transform::from_scale(scale, scale)
                        .post_translate(translate_x, translate_y),
                    scaled_width,
                    scaled_height,
                )
            } else {
                (
                    resvg::tiny_skia::Transform::from_scale(scale_x, scale_y),
                    target_width,
                    target_height,
                )
            };
            log::info!(
                "Scaled SVG to {}x{} (target {}x{}, maintain_aspect_ratio={})",
                scaled_width,
                scaled_height,
                target_width,
                target_height,
                config.maintain_aspect_ratio
            );
            resvg::render(&rtree, transform, &mut buf.as_mut());
            
            // Apply clipping
            let clipped_width = config.width.saturating_sub(config.clip_left + config.clip_right);
            let clipped_height = config.height.saturating_sub(config.clip_top + config.clip_bottom);
            
            let clipped_buffer = if clipped_width > 0 && clipped_height > 0 && 
                                   (config.clip_top > 0 || config.clip_bottom > 0 || 
                                    config.clip_left > 0 || config.clip_right > 0) {
                // Create clipped buffer
                let mut clipped_buf = Vec::with_capacity((clipped_width * clipped_height * 4) as usize);
                let src_data = buf.data();
                
                for y in config.clip_top..(config.clip_top + clipped_height) {
                    if y >= config.height {
                        break;
                    }
                    let src_row_start = (y * config.width + config.clip_left) as usize * 4;
                    let src_row_end = src_row_start + (clipped_width as usize * 4);
                    if src_row_end <= src_data.len() {
                        clipped_buf.extend_from_slice(&src_data[src_row_start..src_row_end]);
                    }
                }
                
                log::info!(
                    "Applied clipping: original {}x{} -> clipped {}x{}",
                    config.width,
                    config.height,
                    clipped_width,
                    clipped_height
                );
                
                clipped_buf
            } else {
                buf.data().to_vec()
            };
            
            *cache_entry.value_mut() = SvgCacheEntry {
                buffer: clipped_buffer,
                width: if clipped_width > 0 { clipped_width } else { config.width },
                height: if clipped_height > 0 { clipped_height } else { config.height },
                ..cache_key
            };
        }

        let cache_entry = cache_entry.value();
        video.set_image_data(&cache_entry.buffer, cache_entry.width, cache_entry.height);
        Ok(())
    }
}

aviutl2::register_generic_plugin!(SvgAux2);
