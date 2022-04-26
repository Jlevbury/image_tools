#![windows_subsystem = "windows"] // Don't go through console on Windows.

use std::path::{Path, PathBuf};

use eframe::{egui, epi};
use egui::containers::Frame;

use sensor_analysis::{utils::lerp_slice, ExposureMapping, Histogram};
use shared_data::Shared;

use lib::ImageInfo;

mod advanced;
mod graph;
mod image_list;
mod menu;
mod simple;
mod tab_bar;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    clap::App::new("ETF LUT Maker")
        .version(VERSION)
        .author("Nathan Vegdahl, Ian Hubert")
        .about("Does all things color space")
        .get_matches();

    eframe::run_native(
        Box::new(AppMain {
            job_queue: job_queue::JobQueue::new(),
            last_opened_directory: None,

            bracket_image_sets: Shared::new(Vec::new()),
            lens_cap_images: Shared::new(Vec::new()),
            transfer_function_tables: Shared::new(None),

            ui_data: Shared::new(UIData {
                image_view: ImageViewID::Bracketed,
                mode: AppMode::Generate,
                preview_mode: graph::PreviewMode::ToLinear,

                selected_bracket_image_index: (0, 0),
                bracket_thumbnail_sets: Vec::new(),

                selected_lens_cap_image_index: 0,
                lens_cap_thumbnails: Vec::new(),

                sensor_floor: [0.0; 3],
                sensor_ceiling: [1.0; 3],
                exposure_mappings: [Vec::new(), Vec::new(), Vec::new()],

                transfer_function_type: TransferFunction::Estimated,
                transfer_function_resolution: 4096,
                normalize_transfer_function: false,
                rounds: 4000,
                transfer_function_preview: None,
            }),
        }),
        eframe::NativeOptions {
            drag_and_drop_support: true, // Enable drag-and-dropping files on Windows.
            ..eframe::NativeOptions::default()
        },
    );
}

pub struct AppMain {
    job_queue: job_queue::JobQueue,
    last_opened_directory: Option<PathBuf>,

    bracket_image_sets: Shared<Vec<Vec<([Histogram; 3], ImageInfo)>>>,
    lens_cap_images: Shared<Vec<[Histogram; 3]>>,
    transfer_function_tables: Shared<Option<([Vec<f32>; 3], f32, f32)>>, // (table, x_min, x_max)

    ui_data: Shared<UIData>,
}

/// The stuff the UI code needs access to for drawing and update.
///
/// Nothing other than the UI should lock this data for non-trivial
/// amounts of time.
pub struct UIData {
    image_view: ImageViewID,
    mode: AppMode,
    preview_mode: graph::PreviewMode,

    selected_bracket_image_index: (usize, usize), // (set index, image index)
    bracket_thumbnail_sets: Vec<Vec<(egui::TextureHandle, usize, usize, ImageInfo)>>, // (tex_handle, width, height, info)

    selected_lens_cap_image_index: usize,
    lens_cap_thumbnails: Vec<(egui::TextureHandle, usize, usize, ImageInfo)>, // (tex_handle, width, height, info)
    sensor_floor: [f32; 3],
    sensor_ceiling: [f32; 3],
    exposure_mappings: [Vec<ExposureMapping>; 3],

    transfer_function_type: TransferFunction,
    transfer_function_resolution: usize,
    normalize_transfer_function: bool,
    rounds: usize,
    transfer_function_preview: Option<([Vec<f32>; 3], f32)>, // (lut, error)
}

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum ImageViewID {
    Bracketed,
    LensCap,
}

impl ImageViewID {
    fn ui_text(&self) -> &'static str {
        match *self {
            ImageViewID::Bracketed => "Bracketed Exposures",
            ImageViewID::LensCap => "Lens Cap Images",
        }
    }
}

impl epi::App for AppMain {
    fn name(&self) -> &str {
        "LUT Maker"
    }

    fn setup(
        &mut self,
        ctx: &egui::Context,
        frame: &epi::Frame,
        _storage: Option<&dyn epi::Storage>,
    ) {
        // Dark mode.
        ctx.set_visuals(egui::style::Visuals {
            dark_mode: true,
            ..egui::style::Visuals::default()
        });

        // Update callback for jobs.
        let frame_clone = frame.clone();
        self.job_queue.set_update_fn(move || {
            frame_clone.request_repaint();
        });
    }

    // Called before shutdown.
    fn save(&mut self, _storage: &mut dyn epi::Storage) {
        // Don't need to do anything.
    }

    fn update(&mut self, ctx: &egui::Context, frame: &epi::Frame) {
        let job_count = self.job_queue.job_count();
        let total_bracket_images: usize = self
            .ui_data
            .lock()
            .bracket_thumbnail_sets
            .iter()
            .map(|s| s.len())
            .sum();
        let total_dark_images: usize = self.ui_data.lock().lens_cap_thumbnails.len();

        let mut working_dir = self
            .last_opened_directory
            .clone()
            .unwrap_or_else(|| "".into());

        //----------------
        // GUI.

        menu::menu_bar(ctx, frame);

        // Status bar and log (footer).
        egui_custom::status_bar(ctx, &self.job_queue);

        // Image list (left-side panel).
        egui::containers::panel::SidePanel::left("image_list")
            .min_width(200.0)
            .resizable(false)
            .show(ctx, |ui| {
                image_list::image_list(ctx, ui, self, job_count, &mut working_dir);
            });

        // Tabs and export buttons.
        egui::containers::panel::TopBottomPanel::top("mode_tabs").show(ctx, |ui| {
            tab_bar::tab_bar(ui, self, job_count, &mut working_dir);
        });

        // Main area.
        egui::containers::panel::CentralPanel::default()
            .frame(
                Frame::none()
                    .stroke(ctx.style().visuals.window_stroke())
                    .margin(egui::style::Margin::same(10.0))
                    .fill(ctx.style().visuals.window_fill()),
            )
            .show(ctx, |ui| {
                // Main UI.
                let mode = self.ui_data.lock().mode;
                match mode {
                    AppMode::Generate => {
                        advanced::advanced_mode_ui(
                            ui,
                            self,
                            job_count,
                            total_bracket_images,
                            total_dark_images,
                        );
                    }
                    AppMode::Estimate => {
                        simple::simple_mode_ui(ui, self, job_count, total_bracket_images);
                    }
                    AppMode::Modify => {}
                }

                ui.add_space(18.0);

                // Graph view.
                graph::graph_ui(ui, self);
            });

        self.last_opened_directory = Some(working_dir);

        //----------------
        // Processing.

        // Collect dropped files.
        if !ctx.input().raw.dropped_files.is_empty() {
            let image_view = self.ui_data.lock().image_view;
            match image_view {
                ImageViewID::Bracketed => self.add_bracket_image_files(
                    ctx.input()
                        .raw
                        .dropped_files
                        .iter()
                        .map(|dropped_file| dropped_file.path.as_ref().unwrap().as_path()),
                    ctx,
                ),
                ImageViewID::LensCap => self.add_lens_cap_image_files(
                    ctx.input()
                        .raw
                        .dropped_files
                        .iter()
                        .map(|dropped_file| dropped_file.path.as_ref().unwrap().as_path()),
                    ctx,
                ),
            }
        }
    }
}

impl AppMain {
    fn add_bracket_image_files<'a, I: Iterator<Item = &'a Path>>(
        &mut self,
        paths: I,
        ctx: &egui::Context,
    ) {
        let mut image_paths: Vec<_> = paths.map(|path| path.to_path_buf()).collect();
        let bracket_image_sets = self.bracket_image_sets.clone_ref();
        let ui_data = self.ui_data.clone_ref();
        let ctx = ctx.clone();

        self.job_queue.add_job("Add Image(s)", move |status| {
            let len = image_paths.len() as f32;

            // Create a new image and thumbnail set.
            bracket_image_sets.lock_mut().push(Vec::new());
            ui_data.lock_mut().bracket_thumbnail_sets.push(Vec::new());

            // Load and add images.
            for (img_i, path) in image_paths.drain(..).enumerate() {
                if status.lock().is_canceled() {
                    break;
                }

                status.lock_mut().set_progress(
                    format!("Loading: {}", path.to_string_lossy()),
                    (img_i + 1) as f32 / len,
                );

                // Load image.
                let img = match lib::job_helpers::load_image(&path) {
                    Ok(img) => img,
                    Err(lib::job_helpers::ImageLoadError::NoAccess) => {
                        status.lock_mut().log_error(format!(
                            "Unable to access file \"{}\".",
                            path.to_string_lossy()
                        ));
                        return;
                    },
                    Err(lib::job_helpers::ImageLoadError::UnknownFormat) => {
                        status.lock_mut().log_error(format!(
                            "Unrecognized image file format: \"{}\".",
                            path.to_string_lossy()
                        ));
                        return;
                    }
                };

                // Ensure it has the same resolution as the other images.
                if !bracket_image_sets.lock().last().unwrap().is_empty() {
                    let needed_width = bracket_image_sets.lock().last().unwrap()[0].1.width as u32;
                    let needed_height = bracket_image_sets.lock().last().unwrap()[0].1.height as u32;
                    if img.image.width() != needed_width || img.image.height() != needed_height {
                        status.lock_mut().log_error(format!(
                            "Image has a different resolution that the others in the set: \"{}\".  Not loading.  Note: all images in a set must have the same resolution.",
                            path.to_string_lossy()
                        ));
                        continue;
                    }
                }

                // Check if we got exposure data from it.
                if img.info.exposure.is_none() {
                    status.lock_mut().log_warning(format!(
                        "Image file lacks Exif data needed to compute exposure value: \"{}\".  Transfer function estimation will not work correctly.",
                        path.to_string_lossy()
                    ));
                }

                // Make a thumbnail texture.
                let (thumbnail_tex_handle, thumbnail_width, thumbnail_height) = {
                    let (pixels, width, height) = lib::job_helpers::make_image_preview(&img, Some(128), None);
                    let tex_handle = ctx.load_texture("",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [width, height],
                                &pixels,
                            ),
                        );
                    (tex_handle, width, height)
                };

                // Compute histograms.
                let histograms = lib::job_helpers::compute_image_histograms(&img, 256);

                // Add image and thumbnail to our lists.
                {
                    let mut ui_data = ui_data.lock_mut();
                    let set = ui_data.bracket_thumbnail_sets.last_mut().unwrap();
                    set.push((thumbnail_tex_handle, thumbnail_width, thumbnail_height, img.info.clone()));
                    set.sort_unstable_by(|a, b| a.3.exposure.partial_cmp(&b.3.exposure).unwrap());
                }
                {
                    let mut bracket_image_sets = bracket_image_sets.lock_mut();
                    let set = bracket_image_sets.last_mut().unwrap();
                    set.push((histograms, img.info.clone()));
                    set.sort_unstable_by(|a, b| a.1.exposure.partial_cmp(&b.1.exposure).unwrap());
                }
            }
        });

        // Update the exposure mappings.
        self.compute_exposure_mappings();
    }

    fn remove_bracket_image(&mut self, set_index: usize, image_index: usize) {
        if set_index >= self.bracket_image_sets.lock().len() {
            return;
        }
        let image_count = self.bracket_image_sets.lock()[set_index].len();
        if image_index >= image_count {
            return;
        }

        // If there won't be any images after this, just remove the
        // whole set.
        if image_count <= 1 {
            self.remove_bracket_image_set(set_index);
            return;
        }

        // Remove the image.
        self.bracket_image_sets.lock_mut()[set_index].remove(image_index);

        // Remove the thumbnail.
        let mut ui_data = self.ui_data.lock_mut();
        let thumbnail_sets = &mut ui_data.bracket_thumbnail_sets;
        if set_index < thumbnail_sets.len() && image_index < thumbnail_sets[set_index].len() {
            let _ = thumbnail_sets[set_index].remove(image_index);
        }

        // Adjust the selected image index appropriately.
        if ui_data.selected_bracket_image_index.0 == set_index
            && ui_data.selected_bracket_image_index.1 > image_index
        {
            ui_data.selected_bracket_image_index.1 -= 1;
        }

        // Update the exposure mappings.
        self.compute_exposure_mappings();
    }

    fn remove_bracket_image_set(&mut self, set_index: usize) {
        {
            // Remove the image set.
            let mut image_sets = self.bracket_image_sets.lock_mut();
            if set_index < image_sets.len() {
                image_sets.remove(set_index);
            }
        }
        {
            // Remove the thumbnail set.
            let mut ui_data = self.ui_data.lock_mut();
            let thumbnail_sets = &mut ui_data.bracket_thumbnail_sets;
            if set_index < thumbnail_sets.len() {
                thumbnail_sets.remove(set_index);
            }

            // Adjust the selected image index appropriately.
            if set_index > thumbnail_sets.len() {
                let new_set_index = thumbnail_sets.len().saturating_sub(1);
                let new_image_index = thumbnail_sets
                    .get(new_set_index)
                    .map(|s| s.len().saturating_sub(1))
                    .unwrap_or(0);
                ui_data.selected_bracket_image_index = (new_set_index, new_image_index);
            } else if set_index == ui_data.selected_bracket_image_index.0 {
                ui_data.selected_bracket_image_index.1 = 0;
            } else if set_index < ui_data.selected_bracket_image_index.0 {
                ui_data.selected_bracket_image_index.0 -= 1;
            }
        }

        // Update the exposure mappings.
        self.compute_exposure_mappings();
    }

    fn add_lens_cap_image_files<'a, I: Iterator<Item = &'a Path>>(
        &mut self,
        paths: I,
        ctx: &egui::Context,
    ) {
        let mut image_paths: Vec<_> = paths.map(|path| path.to_path_buf()).collect();
        let lens_cap_images = self.lens_cap_images.clone_ref();
        let ui_data = self.ui_data.clone_ref();
        let ctx = ctx.clone();

        self.job_queue.add_job("Add Image(s)", move |status| {
            let len = image_paths.len() as f32;

            // Load and add images.
            for (img_i, path) in image_paths.drain(..).enumerate() {
                if status.lock().is_canceled() {
                    break;
                }

                status.lock_mut().set_progress(
                    format!("Loading: {}", path.to_string_lossy()),
                    (img_i + 1) as f32 / len,
                );

                // Load image.
                let img = match lib::job_helpers::load_image(&path) {
                    Ok(img) => img,
                    Err(lib::job_helpers::ImageLoadError::NoAccess) => {
                        status.lock_mut().log_error(format!(
                            "Unable to access file \"{}\".",
                            path.to_string_lossy()
                        ));
                        return;
                    }
                    Err(lib::job_helpers::ImageLoadError::UnknownFormat) => {
                        status.lock_mut().log_error(format!(
                            "Unrecognized image file format: \"{}\".",
                            path.to_string_lossy()
                        ));
                        return;
                    }
                };

                // Make a thumbnail texture.
                let (thumbnail_tex_handle, thumbnail_width, thumbnail_height) = {
                    let (pixels, width, height) =
                        lib::job_helpers::make_image_preview(&img, Some(128), None);
                    let tex_handle = ctx.load_texture(
                        "",
                        egui::ColorImage::from_rgba_unmultiplied([width, height], &pixels),
                    );
                    (tex_handle, width, height)
                };

                // Compute histograms.
                let histograms = lib::job_helpers::compute_image_histograms(&img, 256);

                // Add image and thumbnail to our lists.
                ui_data.lock_mut().lens_cap_thumbnails.push((
                    thumbnail_tex_handle,
                    thumbnail_width,
                    thumbnail_height,
                    img.info.clone(),
                ));
                lens_cap_images.lock_mut().push(histograms);
            }
        });
    }

    fn remove_lens_cap_image(&self, image_index: usize) {
        self.lens_cap_images.lock_mut().remove(image_index);

        let mut ui_data = self.ui_data.lock_mut();
        let _ = ui_data.lens_cap_thumbnails.remove(image_index);
        if ui_data.selected_lens_cap_image_index > image_index {
            ui_data.selected_lens_cap_image_index =
                ui_data.selected_lens_cap_image_index.saturating_sub(1);
        }
    }

    fn estimate_sensor_floor(&self) {
        use sensor_analysis::estimate_sensor_floor_ceiling;

        let bracket_image_sets = self.bracket_image_sets.clone_ref();
        let lens_cap_images = self.lens_cap_images.clone_ref();
        let ui_data = self.ui_data.clone_ref();

        self.job_queue
            .add_job("Estimate Sensor Noise Floor", move |status| {
                status
                    .lock_mut()
                    .set_progress(format!("Estimating sensor noise floor"), 0.0);

                if !lens_cap_images.lock().is_empty() {
                    // Collect stats.
                    let mut sum = [0.0f64; 3];
                    let mut sample_count = [0usize; 3];
                    for histograms in lens_cap_images.lock().iter() {
                        for chan in 0..3 {
                            let norm = 1.0 / (histograms[chan].buckets.len() - 1) as f64;
                            for (i, bucket_population) in
                                histograms[chan].buckets.iter().enumerate()
                            {
                                let v = i as f64 * norm;
                                sum[chan] += v * (*bucket_population as f64);
                                sample_count[chan] += *bucket_population;
                            }
                        }
                    }

                    // Compute floor.
                    for chan in 0..3 {
                        let n = sum[chan] / sample_count[chan].max(1) as f64;
                        ui_data.lock_mut().sensor_floor[chan] = n.max(0.0).min(1.0) as f32;
                    }
                } else {
                    let histogram_sets =
                        bracket_images_to_histogram_sets(&*bracket_image_sets.lock());

                    // Estimate sensor floor for each channel.
                    let mut floor: [Option<f32>; 3] = [None; 3];
                    for histograms in histogram_sets.iter() {
                        if status.lock().is_canceled() {
                            return;
                        }
                        for i in 0..3 {
                            let norm = 1.0 / (histograms[i][0].0.buckets.len() - 1) as f32;
                            if let Some((f, _)) = estimate_sensor_floor_ceiling(&histograms[i]) {
                                if let Some(ref mut floor) = floor[i] {
                                    *floor = floor.min(f * norm);
                                } else {
                                    floor[i] = Some(f * norm);
                                }
                            }
                        }
                    }

                    for i in 0..3 {
                        ui_data.lock_mut().sensor_floor[i] = floor[i].unwrap_or(0.0);
                    }
                }
            });
    }

    fn estimate_sensor_ceiling(&self) {
        use sensor_analysis::estimate_sensor_floor_ceiling;

        let bracket_image_sets = self.bracket_image_sets.clone_ref();
        let ui_data = self.ui_data.clone_ref();

        self.job_queue
            .add_job("Estimate Sensor Ceiling", move |status| {
                status
                    .lock_mut()
                    .set_progress(format!("Estimating sensor ceiling"), 0.0);

                let histogram_sets = bracket_images_to_histogram_sets(&*bracket_image_sets.lock());

                // Estimate sensor floor for each channel.
                let mut ceiling: [Option<f32>; 3] = [None; 3];
                for histograms in histogram_sets.iter() {
                    if status.lock().is_canceled() {
                        return;
                    }
                    for i in 0..3 {
                        let norm = 1.0 / (histograms[i][0].0.buckets.len() - 1) as f32;
                        if let Some((_, c)) = estimate_sensor_floor_ceiling(&histograms[i]) {
                            if let Some(ref mut ceiling) = ceiling[i] {
                                *ceiling = ceiling.max(c * norm);
                            } else {
                                ceiling[i] = Some(c * norm);
                            }
                        }
                    }
                }

                for i in 0..3 {
                    ui_data.lock_mut().sensor_ceiling[i] = ceiling[i].unwrap_or(1.0);
                }
            });
    }

    fn compute_exposure_mappings(&self) {
        let bracket_image_sets = self.bracket_image_sets.clone_ref();
        let ui_data = self.ui_data.clone_ref();

        self.job_queue
            .add_job("Compute Exposure Mappings", move |status| {
                let histogram_sets = bracket_images_to_histogram_sets(&*bracket_image_sets.lock());
                let floor = ui_data.lock().sensor_floor;
                let ceiling = ui_data.lock().sensor_ceiling;

                // Compute exposure mappings.
                status
                    .lock_mut()
                    .set_progress(format!("Computing exposure mappings"), 0.0);
                let mut mappings = [Vec::new(), Vec::new(), Vec::new()];
                for histograms in histogram_sets.iter() {
                    for chan in 0..histograms.len() {
                        for i in 0..histograms[chan].len() {
                            if status.lock().is_canceled() {
                                return;
                            }

                            // Find the histogram with closest to 2x the exposure of this one.
                            const TARGET_RATIO: f32 = 2.0;
                            let mut other_hist_i = i;
                            let mut best_ratio: f32 = -std::f32::INFINITY;
                            for j in (i + 1)..histograms[chan].len() {
                                let ratio = histograms[chan][j].1 / histograms[chan][i].1;
                                if (ratio - TARGET_RATIO).abs() > (best_ratio - TARGET_RATIO).abs()
                                {
                                    break;
                                }
                                other_hist_i = j;
                                best_ratio = ratio;
                            }

                            // Compute and add the exposure mapping.
                            if other_hist_i > i {
                                mappings[chan].push(ExposureMapping::from_histograms(
                                    &histograms[chan][i].0,
                                    &histograms[chan][other_hist_i].0,
                                    histograms[chan][i].1,
                                    histograms[chan][other_hist_i].1,
                                    floor[chan],
                                    ceiling[chan],
                                ));
                            }
                        }
                    }
                }

                ui_data.lock_mut().exposure_mappings = mappings;
            });
    }

    fn estimate_transfer_curve(&self) {
        use sensor_analysis::emor;

        // Make sure the exposure mappings are up-to-date.
        self.compute_exposure_mappings();

        let transfer_function_tables = self.transfer_function_tables.clone_ref();
        let ui_data = self.ui_data.clone_ref();

        self.job_queue
            .add_job("Estimate Transfer Function", move |status| {
                ui_data.lock_mut().transfer_function_type = TransferFunction::Estimated;
                let total_rounds = ui_data.lock().rounds;

                let mappings: Vec<ExposureMapping> = ui_data
                    .lock()
                    .exposure_mappings
                    .clone()
                    .iter()
                    .map(|m| m.clone())
                    .flatten()
                    .collect();
                if mappings.is_empty() {
                    return;
                }

                // Estimate transfer function.
                let rounds_per_update = (1000 / mappings.len()).max(1);
                let mut estimator = emor::EmorEstimator::new(&mappings);
                for round_i in 0..(total_rounds / rounds_per_update) {
                    status.lock_mut().set_progress(
                        format!(
                            "Estimating transfer function, round {}/{}",
                            round_i * rounds_per_update,
                            total_rounds
                        ),
                        (round_i * rounds_per_update) as f32 / total_rounds as f32,
                    );
                    if status.lock().is_canceled() {
                        return;
                    }

                    estimator.do_rounds(rounds_per_update);
                    let (inv_emor_factors, err) = estimator.current_estimate();
                    let mut curves: [Vec<f32>; 3] = [Vec::new(), Vec::new(), Vec::new()];
                    for i in 0..3 {
                        // The (0.0, 1.0) floor/ceil here is because we handle the
                        // floor/ceil adjustment dynamically when previewing and exporting.
                        curves[i] = emor::inv_emor_factors_to_curve(&inv_emor_factors, 0.0, 1.0);
                    }

                    // Store the curve and the preview.
                    *transfer_function_tables.lock_mut() = Some((curves.clone(), 0.0, 1.0));
                    ui_data.lock_mut().transfer_function_preview = Some((curves, err));
                }
            });
    }

    fn estimate_everything(&self) {
        self.estimate_sensor_floor();
        self.estimate_sensor_ceiling();
        self.estimate_transfer_curve();
    }

    fn save_lut(&self, path: &std::path::Path, to_linear: bool) {
        let transfer_function_tables = self.transfer_function_tables.clone_ref();
        let ui_data = self.ui_data.clone_ref();
        let path = path.to_path_buf();

        self.job_queue.add_job("Save LUT", move |status| {
            status
                .lock_mut()
                .set_progress(format!("Saving LUT: {}", path.to_string_lossy(),), 0.0);

            let (function, floor, ceiling, resolution, normalize) = {
                let ui_data = ui_data.lock();
                (
                    ui_data.transfer_function_type,
                    ui_data.sensor_floor,
                    ui_data.sensor_ceiling,
                    ui_data.transfer_function_resolution,
                    ui_data.normalize_transfer_function,
                )
            };

            if floor.iter().zip(ceiling.iter()).any(|(a, b)| *a >= *b) {
                status.lock_mut().log_error(
                    "cannot write a valid LUT file when the sensor floor \
                     has equal or greater values than the ceiling."
                        .into(),
                );
                return;
            }

            // Compute the LUT.
            let lut = if function == TransferFunction::Estimated {
                // Estimated function.
                let (tables, _, _) = transfer_function_tables.lock().clone().unwrap();

                // Build LUT.
                let mut to_linear_lut = colorbox::lut::Lut1D {
                    ranges: vec![(0.0, 1.0)],
                    tables: tables.to_vec(),
                };

                // Apply the floor and ceiling.
                for i in 0..3 {
                    let floor = lerp_slice(&to_linear_lut.tables[i], floor[i]);
                    let ceil = lerp_slice(&to_linear_lut.tables[i], ceiling[i]);
                    let norm = 1.0 / (ceil - floor);
                    for n in to_linear_lut.tables[i].iter_mut() {
                        *n = (*n - floor) * norm;
                    }
                }

                // Invert if needed.
                if to_linear {
                    to_linear_lut
                } else {
                    to_linear_lut.resample_inverted(4096)
                }
            } else if to_linear {
                // Fixed function, to linear.
                let norm = 1.0 / (resolution - 1) as f32;
                colorbox::lut::Lut1D {
                    ranges: vec![(0.0, 1.0)],
                    tables: (0..3)
                        .map(|chan| {
                            (0..resolution)
                                .map(|i| {
                                    function.to_linear_fc(
                                        i as f32 * norm,
                                        floor[chan],
                                        ceiling[chan],
                                        normalize,
                                    )
                                })
                                .collect()
                        })
                        .collect(),
                }
            } else {
                // Fixed function, from linear.
                let range_min = (0..3).fold(std::f32::INFINITY, |a, i| {
                    a.min(function.to_linear_fc(0.0, floor[i], ceiling[i], normalize))
                });
                let range_max = (0..3).fold(-std::f32::INFINITY, |a, i| {
                    a.max(function.to_linear_fc(1.0, floor[i], ceiling[i], normalize))
                });
                let norm = (range_max - range_min) / (resolution - 1) as f32;

                let tables: Vec<Vec<_>> = (0..3)
                    .map(|chan| {
                        (0..resolution)
                            .map(|i| {
                                function
                                    .from_linear_fc(
                                        range_min + (i as f32 * norm),
                                        floor[chan],
                                        ceiling[chan],
                                        normalize,
                                    )
                                    .max(0.0)
                                    .min(1.0)
                            })
                            .collect()
                    })
                    .collect();

                colorbox::lut::Lut1D {
                    ranges: vec![(range_min, range_max)],
                    tables: tables,
                }
            };

            // Write out the LUT.
            let path_ref = &path;
            let write_result = (|| -> std::io::Result<()> {
                match path_ref
                    .extension()
                    .map(|e| e.to_str())
                    .flatten()
                    .unwrap_or_else(|| "")
                {
                    "cube" | "CUBE" => colorbox::formats::cube::write_1d(
                        &mut std::io::BufWriter::new(std::fs::File::create(path_ref)?),
                        [(lut.ranges[0].0, lut.ranges[0].1); 3],
                        [&lut.tables[0], &lut.tables[1], &lut.tables[2]],
                    )?,

                    // Default to spi1d in absence of a known extension.
                    "spi1d" | "SPI1D" | _ => colorbox::formats::spi1d::write(
                        &mut std::io::BufWriter::new(std::fs::File::create(path_ref)?),
                        lut.ranges[0].0,
                        lut.ranges[0].1,
                        &[&lut.tables[0], &lut.tables[1], &lut.tables[2]],
                    )?,
                }
                Ok(())
            })();

            if let Err(_) = write_result {
                status.lock_mut().log_error(format!(
                    "couldn't write to {}.  Please make sure the selected file path is writable.",
                    path.to_string_lossy()
                ));
            }
        });
    }
}

/// Utility function to get histograms into the right order for processing.
fn bracket_images_to_histogram_sets(
    image_sets: &[Vec<([Histogram; 3], ImageInfo)>],
) -> Vec<[Vec<(Histogram, f32)>; 3]> {
    let mut histogram_sets: Vec<[Vec<(Histogram, f32)>; 3]> = Vec::new();
    for images in image_sets.iter() {
        let mut histograms = [Vec::new(), Vec::new(), Vec::new()];
        for src_img in images.iter() {
            for chan in 0..3 {
                if let Some(exposure) = src_img.1.exposure {
                    histograms[chan].push((src_img.0[chan].clone(), exposure));
                }
            }
        }

        histogram_sets.push(histograms);
    }
    histogram_sets
}

//-------------------------------------------------------------

#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum AppMode {
    Generate,
    Estimate,
    Modify,
}

#[allow(non_camel_case_types)]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
enum TransferFunction {
    Estimated,
    Linear,
    CanonLog1,
    CanonLog2,
    CanonLog3,
    DJIDlog,
    FujifilmFlog,
    HLG,
    NikonNlog,
    PanasonicVlog,
    PQ,
    PQ_108,
    PQ_1000,
    Rec709,
    SonySlog1,
    SonySlog2,
    SonySlog3,
    sRGB,
}

const TRANSFER_FUNCTIONS: &[TransferFunction] = &[
    TransferFunction::Estimated,
    TransferFunction::Linear,
    TransferFunction::sRGB,
    TransferFunction::Rec709,
    TransferFunction::HLG,
    TransferFunction::PQ,
    TransferFunction::PQ_108,
    TransferFunction::PQ_1000,
    TransferFunction::CanonLog1,
    TransferFunction::CanonLog2,
    TransferFunction::CanonLog3,
    TransferFunction::DJIDlog,
    TransferFunction::FujifilmFlog,
    TransferFunction::NikonNlog,
    TransferFunction::PanasonicVlog,
    TransferFunction::SonySlog1,
    TransferFunction::SonySlog2,
    TransferFunction::SonySlog3,
];

impl TransferFunction {
    fn to_linear_fc(&self, n: f32, floor: f32, ceil: f32, normalize: bool) -> f32 {
        let (_, _, _, linear_top, _) = self.constants();
        let out_floor = self.to_linear(floor);
        let out_ceil = self.to_linear(ceil);

        let mut out = self.to_linear(n);
        out = (out - out_floor) / (out_ceil - out_floor);
        if !normalize {
            out *= linear_top;
        }

        out
    }

    fn from_linear_fc(&self, mut n: f32, floor: f32, ceil: f32, normalize: bool) -> f32 {
        let (_, _, _, linear_top, _) = self.constants();
        let in_floor = self.to_linear(floor);
        let in_ceil = self.to_linear(ceil);

        if !normalize {
            n /= linear_top;
        }
        n = in_floor + (n * (in_ceil - in_floor));

        self.from_linear(n)
    }

    fn to_linear(&self, n: f32) -> f32 {
        use colorbox::transfer_functions::*;
        use TransferFunction::*;
        match *self {
            Estimated => panic!("No built-in function for an estimated transfer function."),
            Linear => n,

            CanonLog1 => canon_log1::to_linear(n),
            CanonLog2 => canon_log2::to_linear(n),
            CanonLog3 => canon_log3::to_linear(n),
            DJIDlog => dji_dlog::to_linear(n),
            FujifilmFlog => fujifilm_flog::to_linear(n),
            HLG => hlg::to_linear(n),
            NikonNlog => nikon_nlog::to_linear(n),
            PanasonicVlog => panasonic_vlog::to_linear(n),
            PQ => pq::to_linear(n),
            PQ_108 => pq::to_linear(n) * (1.0 / 108.0),
            PQ_1000 => pq::to_linear(n) * (1.0 / 1000.0),
            Rec709 => rec709::to_linear(n),
            SonySlog1 => sony_slog1::to_linear(n),
            SonySlog2 => sony_slog2::to_linear(n),
            SonySlog3 => sony_slog3::to_linear(n),
            sRGB => srgb::to_linear(n),
        }
    }

    fn from_linear(&self, n: f32) -> f32 {
        use colorbox::transfer_functions::*;
        use TransferFunction::*;
        match *self {
            Estimated => panic!("No built-in function for an estimated transfer function."),
            Linear => n,

            CanonLog1 => canon_log1::from_linear(n),
            CanonLog2 => canon_log2::from_linear(n),
            CanonLog3 => canon_log3::from_linear(n),
            DJIDlog => dji_dlog::from_linear(n),
            FujifilmFlog => fujifilm_flog::from_linear(n),
            HLG => hlg::from_linear(n),
            NikonNlog => nikon_nlog::from_linear(n),
            PanasonicVlog => panasonic_vlog::from_linear(n),
            PQ => pq::from_linear(n),
            PQ_108 => pq::from_linear(n * 108.0),
            PQ_1000 => pq::from_linear(n * 1000.0),
            Rec709 => rec709::from_linear(n),
            SonySlog1 => sony_slog1::from_linear(n),
            SonySlog2 => sony_slog2::from_linear(n),
            SonySlog3 => sony_slog3::from_linear(n),
            sRGB => srgb::from_linear(n),
        }
    }

    /// Returns (NONLINEAR_BLACK, NONLINEAR_MAX, LINEAR_MIN, LINEAR_MAX,
    /// LINEAR_SATURATE) for the transfer function.
    ///
    /// - NONLINEAR_BLACK is the non-linear value of linear = 0.0.
    /// - NONLINEAR_MAX is the maximum nonlinear value that should be
    ///   reportable by a camera sensor.  Usually 1.0, but some transfer
    ///   functions are weird.
    /// - LINEAR_MIN/MAX are the linear values when the encoded value is
    ///   0.0 and 1.0.
    /// - LINEAR_SATURATE is the linear value when the encoded value is
    ///   NONLINEAR_MAX.  Usually the same as LINEAR_MAX, but some
    ///   transfer functions are weird.
    #[inline(always)]
    fn constants(&self) -> (f32, f32, f32, f32, f32) {
        use colorbox::transfer_functions::*;
        use TransferFunction::*;
        match *self {
            Estimated => panic!("No built-in function for an estimated transfer function."),
            Linear => (0.0, 1.0, 0.0, 1.0, 1.0),

            CanonLog1 => {
                use canon_log1::*;
                (NONLINEAR_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            CanonLog2 => {
                use canon_log2::*;
                (NONLINEAR_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            CanonLog3 => {
                use canon_log3::*;
                (NONLINEAR_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            DJIDlog => {
                use dji_dlog::*;
                (CV_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            FujifilmFlog => {
                use fujifilm_flog::*;
                (CV_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            HLG => (0.0, 1.0, 0.0, 1.0, 1.0),
            NikonNlog => {
                use nikon_nlog::*;
                (CV_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            PanasonicVlog => {
                use panasonic_vlog::*;
                (CV_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            PQ => (0.0, 1.0, 0.0, pq::LUMINANCE_MAX, pq::LUMINANCE_MAX),
            PQ_108 => (
                0.0,
                1.0,
                0.0,
                pq::LUMINANCE_MAX / 108.0,
                pq::LUMINANCE_MAX / 108.0,
            ),
            PQ_1000 => (
                0.0,
                1.0,
                0.0,
                pq::LUMINANCE_MAX / 1000.0,
                pq::LUMINANCE_MAX / 1000.0,
            ),
            Rec709 => (0.0, 1.0, 0.0, 1.0, 1.0),
            SonySlog1 => {
                use sony_slog1::*;
                (
                    CV_BLACK,
                    CV_SATURATION,
                    LINEAR_MIN,
                    LINEAR_MAX,
                    self.to_linear(CV_SATURATION),
                )
            }
            SonySlog2 => {
                use sony_slog2::*;
                (
                    CV_BLACK,
                    CV_SATURATION,
                    LINEAR_MIN,
                    LINEAR_MAX,
                    self.to_linear(CV_SATURATION),
                )
            }
            SonySlog3 => {
                use sony_slog3::*;
                (CV_BLACK, 1.0, LINEAR_MIN, LINEAR_MAX, LINEAR_MAX)
            }
            sRGB => (0.0, 1.0, 0.0, 1.0, 1.0),
        }
    }

    fn ui_text(&self) -> &'static str {
        use TransferFunction::*;
        match *self {
            Estimated => "Estimated",
            Linear => "Linear",

            CanonLog1 => "Canon Log",
            CanonLog2 => "Canon Log 2",
            CanonLog3 => "Canon Log 3",
            DJIDlog => "DJI D-Log",
            FujifilmFlog => "Fujifilm F-Log",
            HLG => "Rec.2100 - HLG",
            NikonNlog => "Nikon N-Log",
            PanasonicVlog => "Panasonic V-Log",
            PQ => "Rec.2100 - PQ",
            PQ_108 => "Rec.2100 - PQ - 108 nits",
            PQ_1000 => "Rec.2100 - PQ - 1000 nits",
            Rec709 => "Rec.709",
            SonySlog1 => "Sony S-Log",
            SonySlog2 => "Sony S-Log2",
            SonySlog3 => "Sony S-Log3",
            sRGB => "sRGB",
        }
    }
}
