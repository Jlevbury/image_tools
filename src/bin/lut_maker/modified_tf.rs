use std::path::PathBuf;

use sensor_analysis::utils::lerp_slice;

use crate::egui::{self, Ui};

pub struct ModifiedTF {
    pub loaded_lut: Option<(colorbox::lut::Lut1D, colorbox::lut::Lut1D, PathBuf)>, // (to linear, from linear, path)
    pub sensor_floor: [f32; 3],
    pub sensor_ceiling: [f32; 3],
}

impl ModifiedTF {
    pub fn new() -> ModifiedTF {
        ModifiedTF {
            loaded_lut: None,
            sensor_floor: [0.0; 3],
            sensor_ceiling: [1.0; 3],
        }
    }

    /// Returns the LUT with the adjustments made from the modified settings.
    ///
    /// The returned value is an array of (lut, range start, range end) tuples,
    /// one for each channel.
    pub fn adjusted_lut(&self, to_linear: bool) -> Option<[(Vec<f32>, f32, f32); 3]> {
        let floor = self.sensor_floor;
        let ceiling = self.sensor_ceiling;

        let (luts, ranges) = if let Some((ref lut1, ref lut2, _)) = self.loaded_lut {
            if to_linear {
                (&lut1.tables[..], &lut1.ranges[..])
            } else {
                (&lut2.tables[..], &lut2.ranges[..])
            }
        } else {
            return None;
        };

        let mut adjusted_luts = [
            (Vec::new(), 0.0, 1.0),
            (Vec::new(), 0.0, 1.0),
            (Vec::new(), 0.0, 1.0),
        ];
        for chan in 0..3 {
            let lut = if luts.len() >= 3 {
                &luts[chan]
            } else {
                &luts[0]
            };
            let range = if ranges.len() >= luts.len() {
                ranges[chan]
            } else {
                ranges[0]
            };

            if to_linear {
                let floor = (floor[chan] - range.0) / (range.1 - range.0);
                let ceil = (ceiling[chan] - range.0) / (range.1 - range.0);
                let out_floor = lerp_slice(lut, floor);
                let out_ceil = lerp_slice(lut, ceil);
                let out_norm = 1.0 / (out_ceil - out_floor);

                adjusted_luts[chan] = (
                    lut.iter().map(|y| (y - out_floor) * out_norm).collect(),
                    range.0,
                    range.1,
                );
            } else {
                let norm = 1.0 / (ceiling[chan] - floor[chan]);
                adjusted_luts[chan] = (
                    lut.clone(),
                    (range.0 - floor[chan]) * norm,
                    (range.1 - floor[chan]) * norm,
                );
            }
        }

        Some(adjusted_luts)
    }
}

pub fn modified_mode_ui(
    ui: &mut Ui,
    app: &mut crate::AppMain,
    job_count: usize,
    total_bracket_images: usize,
    total_dark_images: usize,
    working_dir: &mut PathBuf,
) {
    let load_1d_lut_dialog = {
        let mut d = rfd::FileDialog::new()
            .set_title("Load 1D LUT")
            .add_filter("All Supported LUTs", &["spi1d", "cube"])
            .add_filter("cube", &["cube"])
            .add_filter("spi1d", &["spi1d"]);
        if !working_dir.as_os_str().is_empty() && working_dir.is_dir() {
            d = d.set_directory(&working_dir);
        }
        d
    };

    // Transfer function controls.
    let area_width = ui.available_width();
    let sub_area_width = (area_width / 3.0).min(230.0);
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.label("LUT");
            if app.ui_data.lock().modified.loaded_lut.is_some() {
                ui.horizontal(|ui| {
                    ui.strong(
                        if let Some(name) = app
                            .ui_data
                            .lock()
                            .modified
                            .loaded_lut
                            .as_ref()
                            .unwrap()
                            .2
                            .file_name()
                        {
                            let tmp: String = name.to_string_lossy().into();
                            tmp
                        } else {
                            "Unnamed LUT".into()
                        },
                    );
                    if ui
                        .add_enabled(job_count == 0, egui::widgets::Button::new("🗙"))
                        .clicked()
                    {
                        app.ui_data.lock_mut().modified.loaded_lut = None;
                    }
                });
                if ui
                    .add_enabled(job_count == 0, egui::widgets::Button::new("Flip LUT"))
                    .clicked()
                {
                    if let Some((ref mut lut1, ref mut lut2, _)) =
                        app.ui_data.lock_mut().modified.loaded_lut
                    {
                        std::mem::swap(lut1, lut2);
                    }
                }
            } else {
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(job_count == 0, egui::widgets::Button::new("Load 1D LUT..."))
                        .clicked()
                    {
                        if let Some(path) = load_1d_lut_dialog.clone().pick_file() {
                            app.load_lut(&path);
                            if let Some(parent) = path.parent().map(|p| p.into()) {
                                *working_dir = parent;
                            }
                        }
                    }
                });
            }
        });

        ui.add_space(48.0);

        // Sensor floor controls.
        ui.vertical(|ui| {
            ui.set_width(sub_area_width);

            ui.horizontal(|ui| {
                ui.label("Sensor Noise Floor");
                ui.add_space(4.0);
                if ui
                    .add_enabled(
                        job_count == 0 && (total_bracket_images > 0 || total_dark_images > 0),
                        egui::widgets::Button::new("Estimate"),
                    )
                    .clicked()
                {
                    app.estimate_sensor_floor();
                }
            });
            ui.add_space(4.0);
            for (label, value) in ["R: ", "G: ", "B: "]
                .iter()
                .zip(app.ui_data.lock_mut().modified.sensor_floor.iter_mut())
            {
                ui.horizontal(|ui| {
                    ui.label(*label);
                    ui.add_enabled(
                        job_count == 0,
                        egui::widgets::Slider::new(value, 0.0..=1.0)
                            .max_decimals(5)
                            .min_decimals(5),
                    );
                });
            }
        });

        ui.add_space(0.0);

        // Sensor ceiling controls.
        ui.vertical(|ui| {
            ui.set_width(sub_area_width);

            ui.horizontal(|ui| {
                ui.label("Sensor Ceiling");
                ui.add_space(4.0);
                if ui
                    .add_enabled(
                        job_count == 0 && total_bracket_images > 0,
                        egui::widgets::Button::new("Estimate"),
                    )
                    .clicked()
                {
                    app.estimate_sensor_ceiling();
                }
            });
            ui.add_space(4.0);
            for (label, value) in ["R: ", "G: ", "B: "]
                .iter()
                .zip(app.ui_data.lock_mut().modified.sensor_ceiling.iter_mut())
            {
                ui.horizontal(|ui| {
                    ui.label(*label);
                    ui.add_enabled(
                        job_count == 0,
                        egui::widgets::Slider::new(value, 0.0..=1.0)
                            .max_decimals(5)
                            .min_decimals(5),
                    );
                });
            }
        });
    });
}
