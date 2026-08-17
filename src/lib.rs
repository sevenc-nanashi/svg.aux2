use aviutl2::{
    anyhow::{self, Context},
    filter::{FilterConfigItemSliceExt, FilterConfigItems},
    tracing,
};

#[aviutl2::plugin(GenericPlugin)]
struct SvgAux2 {
    filter: aviutl2::generic::SubPlugin<SvgFilter>,
}

static EDIT_HANDLE: aviutl2::generic::GlobalEditHandle = aviutl2::generic::GlobalEditHandle::new();

impl aviutl2::generic::GenericPlugin for SvgAux2 {
    fn new(info: aviutl2::AviUtl2Info) -> aviutl2::AnyResult<Self> {
        Ok(Self {
            filter: aviutl2::generic::SubPlugin::new_filter_plugin(&info)?,
        })
    }

    fn plugin_info(&self) -> aviutl2::generic::GenericPluginTable {
        aviutl2::generic::GenericPluginTable {
            name: "svg.aux2".to_string(),
            information: format!(
                "Render SVG files as filter objects / v{} / https://github.com/sevenc-nanashi/svg.aux2",
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    fn register(&mut self, registry: &mut aviutl2::generic::HostAppHandle) {
        registry.register_filter_plugin(&self.filter);
        let filters = aviutl2::file_filters! {
            "SVG" => ["svg"]
        };
        EDIT_HANDLE.init(registry.create_edit_handle());
        registry.register_file_drop_handler("svg.aux2", &filters, |file| {
            let res = EDIT_HANDLE
                .call_edit_section(|e| {
                    let position = e.get_mouse_layer_frame()?.unwrap_or({
                        aviutl2::generic::LayerFrameData {
                            layer: e.info.layer,
                            frame: e.info.frame,
                        }
                    });

                    let mut object_alias = aviutl2::alias::Table::new();
                    let mut object = aviutl2::alias::Table::new();
                    object.insert_value(
                        "name",
                        file.file_name().and_then(|n| n.to_str()).unwrap_or("SVG"),
                    );
                    let mut object_0 = aviutl2::alias::Table::new();
                    object_0.insert_value("effect.name", "SVG");
                    object_0.insert_value(
                        "ファイル",
                        file.to_str()
                            .context("Failed to convert file path to string for object alias")?,
                    );
                    object.insert_table("0", object_0);
                    object_alias.insert_table("Object", object);

                    e.create_object_from_alias(
                        &object_alias.to_string(),
                        position.layer,
                        position.frame,
                        0,
                    )?;

                    anyhow::Ok(())
                })
                .map_err(anyhow::Error::from)
                .flatten();
            if let Err(e) = res {
                tracing::error!("Failed to handle file drop: {}", e);
            }
        })
    }
}

#[derive(Clone, Hash)]
enum SvgSource {
    File(std::path::PathBuf),
    Inline(String),
}
impl std::fmt::Debug for SvgSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        struct NumBytes(usize);
        impl std::fmt::Debug for NumBytes {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                if self.0 < 1024 {
                    write!(f, "{} bytes", self.0)
                } else {
                    write!(f, "{:.2} KB", self.0 as f64 / 1024.0)
                }
            }
        }
        match self {
            SvgSource::File(path) => f.debug_tuple("File").field(path).finish(),
            SvgSource::Inline(data) => f
                .debug_tuple("Inline")
                .field(&NumBytes(data.len()))
                .finish(),
        }
    }
}

#[derive(Debug, Clone, Hash)]
struct SvgParam {
    source: SvgSource,
    color: (u8, u8, u8),

    width: u32,
    height: u32,
    maintain_aspect_ratio: bool,
    clipping: (u32, u32, u32, u32),
}

static FONT_DB: std::sync::LazyLock<std::sync::Arc<resvg::usvg::fontdb::Database>> =
    std::sync::LazyLock::new(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        std::sync::Arc::new(db)
    });

fn unpremultiply_rgba(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel[..3].fill(0);
            continue;
        }

        for channel in &mut pixel[..3] {
            let straight = (u16::from(*channel) * 255 + alpha / 2) / alpha;
            *channel = straight.min(255) as u8;
        }
    }
}

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
    #[group(name = "インライン入力", opened = false)]
    inline_input: group! {
        #[text(name = "SVGコード", default = "")]
        svg_data: String,
    },
}

impl aviutl2::filter::FilterPlugin for SvgFilter {
    fn new(_info: aviutl2::AviUtl2Info) -> aviutl2::AnyResult<Self> {
        aviutl2::tracing_subscriber::fmt()
            .with_max_level(if cfg!(debug_assertions) {
                tracing::Level::DEBUG
            } else {
                tracing::Level::INFO
            })
            .event_format(aviutl2::logger::AviUtl2Formatter)
            .with_writer(aviutl2::logger::AviUtl2LogWriter)
            .init();
        Ok(Self {})
    }

    fn plugin_info(&self) -> aviutl2::filter::FilterPluginTable {
        aviutl2::filter::FilterPluginTable {
            name: "SVG".into(),
            label: None,
            flags: aviutl2::bitflag!(aviutl2::filter::FilterPluginFlags {
                video: true,
                input: true
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
        let source = match (config.svg_data.as_str(), config.svg_file.as_ref()) {
            (inline, _) if !inline.trim().is_empty() => SvgSource::Inline(inline.to_string()),
            (_, Some(path)) => SvgSource::File(path.clone()),
            _ => return Ok(()),
        };
        let color = config.color.to_rgb();

        let debug_source = format!("{:?}", &source);
        let cache_key = SvgParam {
            source,
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
        };
        let cache_hash = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            "straight-alpha-v1".hash(&mut hasher);
            cache_key.hash(&mut hasher);
            hasher.finish().to_string()
        };
        let cache_entry =
            aviutl2::cache::get_image_cache(&aviutl2::cache::GLOBAL_CACHE_HANDLE, &cache_hash)?;
        if let Some(cache_entry) = cache_entry {
            tracing::debug!("Cache hit for SVG {debug_source} with hash {}", cache_hash);
            video.set_image_data(
                cache_entry.as_u8_slice(),
                cache_entry.width() as _,
                cache_entry.height() as _,
            );
            return Ok(());
        }
        tracing::info!(
            "Rendering SVG {} with color rgb({},{},{}) at size {}x{}",
            debug_source,
            color.0,
            color.1,
            color.2,
            config.width,
            config.height
        );
        let svg_data = match &cache_key.source {
            SvgSource::File(path) => std::fs::read_to_string(path).map_err(|e| {
                anyhow::anyhow!("Failed to read SVG file '{}': {}", path.display(), e)
            })?,
            SvgSource::Inline(data) => data.clone(),
        };
        let opt = resvg::usvg::Options {
            style_sheet: Some(format!(
                "* {{ color: rgb({},{},{}); }}",
                color.0, color.1, color.2
            )),
            fontdb: std::sync::Arc::clone(&FONT_DB),
            ..Default::default()
        };
        let rtree = resvg::usvg::Tree::from_str(&svg_data, &opt).map_err(|e| {
            anyhow::anyhow!(
                "Failed to create usvg Tree from SVG data for source {:?}: {}",
                cache_key.source,
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
        tracing::debug!(
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
        tracing::debug!("Canvas size: {}x{}", canvas_width, canvas_height);
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
        unpremultiply_rgba(buf.data_mut());

        let mut cache_entry = aviutl2::cache::create_image_cache(
            &aviutl2::cache::GLOBAL_CACHE_HANDLE,
            &cache_hash,
            buf.width() as _,
            buf.height() as _,
        )?;
        cache_entry.as_u8_slice_mut().copy_from_slice(buf.data());
        video.set_image_data(
            cache_entry.as_u8_slice(),
            cache_entry.width() as _,
            cache_entry.height() as _,
        );
        Ok(())
    }
}

aviutl2::register_generic_plugin!(SvgAux2);
