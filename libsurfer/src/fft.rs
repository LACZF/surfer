use egui::{Context, Ui};
use egui_plot::{Legend, Line, Plot, PlotPoints, Points};
use egui_remixicon::icons;
use num::{BigInt, BigUint, ToPrimitive};
use rustfft::{FftPlanner, num_complex::Complex};

use crate::SystemState;
use crate::displayed_item::{DisplayedFieldRef, DisplayedItem, DisplayedItemRef};
use crate::message::Message;
use crate::time::TimeInputState;
use crate::translation::TranslatorList;
use crate::wave_container::VariableRefExt;
use crate::wave_data::WaveData;

const DEFAULT_SAMPLE_COUNT: u64 = 1024;
const MAX_FFT_SAMPLES: usize = 1_048_576;
const MIN_MAGNITUDE_DB: f64 = -120.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FrequencyPlot {
    MagnitudePhase,
    RealImaginary,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SamplingMode {
    SampleTime,
    RisingEdge,
}

pub(crate) struct FftState {
    start: TimeInputState,
    stop: TimeInputState,
    sample_time: TimeInputState,
    sampling_mode: SamplingMode,
    clock_signal: Option<DisplayedItemRef>,
    clock_offset: TimeInputState,
    edge_multiple: usize,
    zero_padding: usize,
    real_signal: Option<DisplayedItemRef>,
    imaginary_signal: Option<DisplayedItemRef>,
    frequency_plot: FrequencyPlot,
    show_magnitude: bool,
    magnitude_db: bool,
    magnitude_stems: bool,
    show_phase: bool,
    unwrap_phase: bool,
    frequency_stems: bool,
    show_time: bool,
    time_stems: bool,
}

impl Default for FftState {
    fn default() -> Self {
        Self {
            start: TimeInputState::default(),
            stop: TimeInputState::default(),
            sample_time: TimeInputState::default(),
            sampling_mode: SamplingMode::SampleTime,
            clock_signal: None,
            clock_offset: TimeInputState::default(),
            edge_multiple: 1,
            zero_padding: 0,
            real_signal: None,
            imaginary_signal: None,
            frequency_plot: FrequencyPlot::MagnitudePhase,
            show_magnitude: true,
            magnitude_db: false,
            magnitude_stems: false,
            show_phase: true,
            unwrap_phase: false,
            frequency_stems: false,
            show_time: true,
            time_stems: false,
        }
    }
}

impl FftState {
    fn initialize(
        &mut self,
        waves: &WaveData,
        options: &[SignalOption],
        clock_options: &[SignalOption],
    ) {
        let timescale = waves.inner.metadata().timescale.clone();
        let stop = waves.safe_max_timestamp();
        let sample_time = (stop.clone() / BigInt::from(DEFAULT_SAMPLE_COUNT)).max(BigInt::from(1));

        if self.start.is_empty() {
            self.start.set_timescale_ticks(&BigInt::from(0), &timescale);
        }
        if self.stop.is_empty() {
            self.stop.set_timescale_ticks(&stop, &timescale);
        }
        if self.sample_time.is_empty() {
            self.sample_time
                .set_timescale_ticks(&sample_time, &timescale);
        }
        if self.clock_offset.is_empty() {
            self.clock_offset
                .set_timescale_ticks(&BigInt::from(0), &timescale);
        }
        self.edge_multiple = self.edge_multiple.max(1);

        if !options
            .iter()
            .any(|option| Some(option.id) == self.real_signal)
        {
            self.real_signal = options.first().map(|option| option.id);
        }
        if self
            .imaginary_signal
            .is_some_and(|id| !options.iter().any(|option| option.id == id))
        {
            self.imaginary_signal = None;
        }
        if !clock_options
            .iter()
            .any(|option| Some(option.id) == self.clock_signal)
        {
            self.clock_signal = clock_options.first().map(|option| option.id);
        }
    }
}

struct SignalOption {
    id: DisplayedItemRef,
    name: String,
}

struct Spectrum {
    sample_times: Vec<f64>,
    real_samples: Vec<f64>,
    imaginary_samples: Option<Vec<f64>>,
    fft_real: Vec<[f64; 2]>,
    fft_imaginary: Vec<[f64; 2]>,
    magnitude: Vec<[f64; 2]>,
    phase: Vec<[f64; 2]>,
    sample_count: usize,
    fft_length: usize,
}

impl SystemState {
    pub(crate) fn draw_fft_window(&mut self, ctx: &Context, msgs: &mut Vec<Message>) {
        let Some(waves) = self.user.waves.as_ref() else {
            return;
        };

        let options = signal_options(waves);
        let clock_options = binary_signal_options(waves);
        let mut fft_state = std::mem::take(&mut self.fft);
        fft_state.initialize(waves, &options, &clock_options);
        let mut open = true;

        egui::Window::new("FFT")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_size((900., 650.))
            .show(ctx, |ui| {
                self.draw_fft_contents(ui, waves, &options, &clock_options, &mut fft_state, msgs);
            });

        self.fft = fft_state;
        if !open {
            msgs.push(Message::SetFftVisible(false));
        }
    }

    fn draw_fft_contents(
        &self,
        ui: &mut Ui,
        waves: &WaveData,
        options: &[SignalOption],
        clock_options: &[SignalOption],
        state: &mut FftState,
        msgs: &mut Vec<Message>,
    ) {
        let mut zoom_to_fit = false;

        ui.horizontal_wrapped(|ui| {
            ui.label("Real signal");
            signal_combo(
                ui,
                "fft-real-signal",
                &mut state.real_signal,
                options,
                false,
            );
            ui.label("Imaginary signal");
            signal_combo(
                ui,
                "fft-imaginary-signal",
                &mut state.imaginary_signal,
                options,
                true,
            );
        });

        ui.horizontal_wrapped(|ui| {
            draw_time_input_row(
                self,
                ui,
                "Start",
                "fft-start",
                waves,
                &mut state.start,
                msgs,
                true,
            );
            draw_time_input_row(
                self,
                ui,
                "Stop",
                "fft-stop",
                waves,
                &mut state.stop,
                msgs,
                true,
            );
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Sampling");
                    let sampling_label = match state.sampling_mode {
                        SamplingMode::SampleTime => "Sample time",
                        SamplingMode::RisingEdge => "Rising edge",
                    };
                    egui::ComboBox::from_id_salt("fft-sampling-mode")
                        .selected_text(sampling_label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(
                                &mut state.sampling_mode,
                                SamplingMode::SampleTime,
                                "Sample time",
                            );
                            ui.selectable_value(
                                &mut state.sampling_mode,
                                SamplingMode::RisingEdge,
                                "Rising edge",
                            );
                        });
                });
                match state.sampling_mode {
                    SamplingMode::SampleTime => {
                        draw_time_input_row(
                            self,
                            ui,
                            "Sample time",
                            "fft-sample-time",
                            waves,
                            &mut state.sample_time,
                            msgs,
                            false,
                        );
                    }
                    SamplingMode::RisingEdge => {
                        ui.horizontal(|ui| {
                            ui.label("Clock signal");
                            signal_combo(
                                ui,
                                "fft-clock-signal",
                                &mut state.clock_signal,
                                clock_options,
                                false,
                            );
                        });
                        draw_time_input_row(
                            self,
                            ui,
                            "Time offset",
                            "fft-clock-offset",
                            waves,
                            &mut state.clock_offset,
                            msgs,
                            false,
                        );
                        ui.horizontal(|ui| {
                            ui.label("Every");
                            ui.add(
                                egui::DragValue::new(&mut state.edge_multiple)
                                    .range(1..=MAX_FFT_SAMPLES)
                                    .speed(1),
                            );
                            ui.label("rising edge");
                        });
                    }
                }
            });
        });

        ui.horizontal(|ui| {
            ui.label("Zero padding");
            ui.add(
                egui::DragValue::new(&mut state.zero_padding)
                    .range(0..=MAX_FFT_SAMPLES)
                    .speed(1),
            );
            ui.checkbox(&mut state.show_time, "Time");
            ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                ui.checkbox(&mut state.time_stems, "Stem plot");
            })
            .response
            .on_hover_text("Time plot settings");
            if ui
                .button("Zoom to fit")
                .on_hover_text("Reset the visible plots to fit all FFT bins")
                .clicked()
            {
                zoom_to_fit = true;
            }
        });

        ui.horizontal_wrapped(|ui| {
            ui.label("Frequency plot");
            let frequency_plot_label = match state.frequency_plot {
                FrequencyPlot::MagnitudePhase => "Magnitude / phase",
                FrequencyPlot::RealImaginary => "Real / imaginary",
            };
            egui::ComboBox::from_id_salt("fft-frequency-plot")
                .selected_text(frequency_plot_label)
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut state.frequency_plot,
                        FrequencyPlot::MagnitudePhase,
                        "Magnitude / phase",
                    );
                    ui.selectable_value(
                        &mut state.frequency_plot,
                        FrequencyPlot::RealImaginary,
                        "Real / imaginary",
                    );
                });
            match state.frequency_plot {
                FrequencyPlot::MagnitudePhase => {
                    ui.checkbox(&mut state.show_magnitude, "Magnitude");
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.magnitude_db, "Magnitude in dB");
                        ui.checkbox(&mut state.magnitude_stems, "Stem plot");
                    })
                    .response
                    .on_hover_text("Magnitude plot settings");
                    ui.checkbox(&mut state.show_phase, "Phase");
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.unwrap_phase, "Unwrap phase");
                    })
                    .response
                    .on_hover_text("Phase plot settings");
                }
                FrequencyPlot::RealImaginary => {
                    ui.menu_button(icons::SETTINGS_3_LINE, |ui| {
                        ui.checkbox(&mut state.frequency_stems, "Stem plot");
                    })
                    .response
                    .on_hover_text("Frequency plot settings");
                }
            }
        });

        ui.separator();

        match calculate_spectrum(waves, &self.translators, state) {
            Ok(spectrum) => {
                ui.label(format!(
                    "{} samples, FFT length {}",
                    spectrum.sample_count, spectrum.fft_length
                ));
                let frequency_plot_count = match state.frequency_plot {
                    FrequencyPlot::MagnitudePhase => {
                        usize::from(state.show_magnitude) + usize::from(state.show_phase)
                    }
                    FrequencyPlot::RealImaginary => 1,
                };
                if !state.show_time && frequency_plot_count == 0 {
                    ui.label("Select a plot to display.");
                } else {
                    let plot_count = usize::from(state.show_time) + frequency_plot_count;
                    let plot_height = if plot_count == 1 { 400.0 } else { 220.0 };

                    if state.show_time {
                        let real_points = spectrum
                            .sample_times
                            .iter()
                            .copied()
                            .zip(spectrum.real_samples.iter().copied())
                            .map(|(time, value)| [time, value])
                            .collect::<Vec<_>>();
                        let imaginary_points = spectrum.imaginary_samples.as_ref().map(|samples| {
                            spectrum
                                .sample_times
                                .iter()
                                .copied()
                                .zip(samples.iter().copied())
                                .map(|(time, value)| [time, value])
                                .collect::<Vec<_>>()
                        });
                        let mut time_plot = Plot::new("fft-time");
                        if zoom_to_fit {
                            time_plot = time_plot.reset();
                        }
                        time_plot
                            .height(plot_height)
                            .legend(Legend::default())
                            .show_axes([true, true])
                            .show_grid([true, true])
                            .x_axis_label("Time from start (s)")
                            .y_axis_label("Sample value")
                            .show(ui, |plot_ui| {
                                if state.time_stems {
                                    plot_ui.points(
                                        Points::new("Real", PlotPoints::new(real_points))
                                            .stems(0.0)
                                            .radius(1.5),
                                    );
                                    if let Some(imaginary_points) = imaginary_points {
                                        plot_ui.points(
                                            Points::new(
                                                "Imaginary",
                                                PlotPoints::new(imaginary_points),
                                            )
                                            .stems(0.0)
                                            .radius(1.5),
                                        );
                                    }
                                } else {
                                    plot_ui.line(Line::new("Real", PlotPoints::new(real_points)));
                                    if let Some(imaginary_points) = imaginary_points {
                                        plot_ui.line(Line::new(
                                            "Imaginary",
                                            PlotPoints::new(imaginary_points),
                                        ));
                                    }
                                }
                            });
                    }

                    match state.frequency_plot {
                        FrequencyPlot::MagnitudePhase => {
                            let magnitude_label = if state.magnitude_db {
                                "Magnitude (dB)"
                            } else {
                                "Magnitude"
                            };
                            if state.show_magnitude {
                                let magnitude_points = spectrum
                                    .magnitude
                                    .iter()
                                    .map(|[frequency, magnitude]| {
                                        [
                                            *frequency,
                                            if state.magnitude_db {
                                                magnitude_to_db(*magnitude)
                                            } else {
                                                *magnitude
                                            },
                                        ]
                                    })
                                    .collect::<Vec<_>>();
                                let mut magnitude_plot = Plot::new("fft-magnitude");
                                if zoom_to_fit {
                                    magnitude_plot = magnitude_plot.reset();
                                }
                                magnitude_plot
                                    .height(plot_height)
                                    .legend(Legend::default())
                                    .show_axes([true, true])
                                    .show_grid([true, true])
                                    .x_axis_label("Frequency (Hz)")
                                    .y_axis_label(magnitude_label)
                                    .show(ui, |plot_ui| {
                                        if state.magnitude_stems {
                                            let stem_reference = if state.magnitude_db {
                                                MIN_MAGNITUDE_DB as f32
                                            } else {
                                                0.0
                                            };
                                            plot_ui.points(
                                                Points::new(
                                                    magnitude_label,
                                                    PlotPoints::new(magnitude_points),
                                                )
                                                .stems(stem_reference)
                                                .radius(1.5),
                                            );
                                        } else {
                                            plot_ui.line(Line::new(
                                                magnitude_label,
                                                PlotPoints::new(magnitude_points),
                                            ));
                                        }
                                    });
                            }

                            if state.show_phase {
                                let phase_points = if state.unwrap_phase {
                                    unwrap_phase(&spectrum.phase)
                                } else {
                                    spectrum.phase.clone()
                                };
                                let phase_label = if state.unwrap_phase {
                                    "Unwrapped phase (rad)"
                                } else {
                                    "Phase (rad)"
                                };
                                let mut phase_plot = Plot::new("fft-phase");
                                if zoom_to_fit {
                                    phase_plot = phase_plot.reset();
                                }
                                phase_plot
                                    .height(plot_height)
                                    .legend(Legend::default())
                                    .show_axes([true, true])
                                    .show_grid([true, true])
                                    .x_axis_label("Frequency (Hz)")
                                    .y_axis_label(phase_label)
                                    .show(ui, |plot_ui| {
                                        plot_ui.line(Line::new(
                                            phase_label,
                                            PlotPoints::new(phase_points),
                                        ));
                                    });
                            }
                        }
                        FrequencyPlot::RealImaginary => {
                            let mut frequency_plot = Plot::new("fft-real-imaginary");
                            if zoom_to_fit {
                                frequency_plot = frequency_plot.reset();
                            }
                            frequency_plot
                                .height(plot_height)
                                .legend(Legend::default())
                                .show_axes([true, true])
                                .show_grid([true, true])
                                .x_axis_label("Frequency (Hz)")
                                .y_axis_label("FFT value")
                                .show(ui, |plot_ui| {
                                    if state.frequency_stems {
                                        plot_ui.points(
                                            Points::new(
                                                "Real",
                                                PlotPoints::new(spectrum.fft_real.clone()),
                                            )
                                            .stems(0.0)
                                            .radius(1.5),
                                        );
                                        plot_ui.points(
                                            Points::new(
                                                "Imaginary",
                                                PlotPoints::new(spectrum.fft_imaginary.clone()),
                                            )
                                            .stems(0.0)
                                            .radius(1.5),
                                        );
                                    } else {
                                        plot_ui.line(Line::new(
                                            "Real",
                                            PlotPoints::new(spectrum.fft_real.clone()),
                                        ));
                                        plot_ui.line(Line::new(
                                            "Imaginary",
                                            PlotPoints::new(spectrum.fft_imaginary.clone()),
                                        ));
                                    }
                                });
                        }
                    }
                }
            }
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }
    }
}

fn magnitude_to_db(magnitude: f64) -> f64 {
    if magnitude > 0.0 {
        (20.0 * magnitude.log10()).max(MIN_MAGNITUDE_DB)
    } else {
        MIN_MAGNITUDE_DB
    }
}

fn unwrap_phase(points: &[[f64; 2]]) -> Vec<[f64; 2]> {
    let mut unwrapped = Vec::with_capacity(points.len());
    let mut phase_offset = 0.0;
    let mut previous_phase = None;

    for [frequency, phase] in points {
        if let Some(previous_phase) = previous_phase {
            let difference = phase - previous_phase;
            if difference > std::f64::consts::PI {
                phase_offset -= std::f64::consts::TAU;
            } else if difference < -std::f64::consts::PI {
                phase_offset += std::f64::consts::TAU;
            }
        }

        unwrapped.push([*frequency, phase + phase_offset]);
        previous_phase = Some(*phase);
    }

    unwrapped
}

fn signal_options(waves: &WaveData) -> Vec<SignalOption> {
    waves
        .items_tree
        .iter_visible()
        .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
            Some(DisplayedItem::Variable(variable)) if variable.analog.is_some() => {
                Some(SignalOption {
                    id: node.item_ref,
                    name: variable.variable_ref.full_path_string(),
                })
            }
            _ => None,
        })
        .collect()
}

fn binary_signal_options(waves: &WaveData) -> Vec<SignalOption> {
    let Some(wave_container) = waves.inner.as_waves() else {
        return vec![];
    };

    waves
        .items_tree
        .iter_visible()
        .filter_map(|node| match waves.displayed_items.get(&node.item_ref) {
            Some(DisplayedItem::Variable(variable)) => {
                let is_binary = wave_container
                    .variable_meta(&variable.variable_ref)
                    .ok()
                    .is_some_and(|meta| meta.num_bits == Some(1));
                is_binary.then(|| SignalOption {
                    id: node.item_ref,
                    name: variable.variable_ref.full_path_string(),
                })
            }
            _ => None,
        })
        .collect()
}

fn signal_combo(
    ui: &mut Ui,
    id: &str,
    selected: &mut Option<DisplayedItemRef>,
    options: &[SignalOption],
    allow_none: bool,
) {
    let selected_text = selected
        .and_then(|id| options.iter().find(|option| option.id == id))
        .map_or_else(
            || {
                if allow_none {
                    "None".to_string()
                } else {
                    "Select a signal".to_string()
                }
            },
            |option| option.name.clone(),
        );

    egui::ComboBox::from_id_salt(id)
        .selected_text(selected_text)
        .show_ui(ui, |ui| {
            if allow_none {
                ui.selectable_value(selected, None, "None");
            }
            for option in options {
                ui.selectable_value(selected, Some(option.id), &option.name);
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn draw_time_input_row(
    state_owner: &SystemState,
    ui: &mut Ui,
    label: &str,
    id: &str,
    waves: &WaveData,
    state: &mut TimeInputState,
    msgs: &mut Vec<Message>,
    show_set_from: bool,
) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.label(label);
            state_owner.time_input_controls(ui, state, id, waves, msgs, None);
            if show_set_from {
                ui.menu_button("Set from", |ui| {
                    let timescale = waves.inner.metadata().timescale.clone();
                    if let Some(cursor) = waves.cursor.as_ref()
                        && ui.button("Cursor").clicked()
                    {
                        state.set_timescale_ticks(cursor, &timescale);
                        ui.close();
                    }

                    let mut markers = waves
                        .markers
                        .iter()
                        .map(|(idx, time)| (*idx, time.clone(), marker_label(waves, *idx)))
                        .collect::<Vec<_>>();
                    markers.sort_by_key(|(idx, _, _)| *idx);
                    for (_, time, marker) in markers {
                        if ui.button(marker).clicked() {
                            state.set_timescale_ticks(&time, &timescale);
                            ui.close();
                        }
                    }
                });
            }
        });
        if let Some(error) = state.error_message() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
    });
}

fn marker_label(waves: &WaveData, idx: u8) -> String {
    let name = waves.displayed_items.values().find_map(|item| match item {
        DisplayedItem::Marker(marker) if marker.idx == idx => marker.name.as_deref(),
        _ => None,
    });
    name.map_or_else(
        || format!("Marker {idx}"),
        |name| format!("Marker {idx} ({name})"),
    )
}

fn calculate_spectrum(
    waves: &WaveData,
    translators: &TranslatorList,
    state: &FftState,
) -> Result<Spectrum, String> {
    let timescale = waves.inner.metadata().timescale.clone();
    let start = state
        .start
        .to_timescale_ticks(&timescale)
        .and_then(|value| value.to_u64())
        .ok_or_else(|| "Enter a valid non-negative start time.".to_string())?;
    let stop = state
        .stop
        .to_timescale_ticks(&timescale)
        .and_then(|value| value.to_u64())
        .ok_or_else(|| "Enter a valid non-negative stop time.".to_string())?;
    let sample_time = state
        .sample_time
        .to_timescale_ticks(&timescale)
        .and_then(|value| value.to_u64())
        .ok_or_else(|| "Enter a valid positive sample time.".to_string())?;

    if sample_time == 0 {
        return Err("Sample time must be greater than zero.".to_string());
    }
    if stop < start {
        return Err("Stop time must not be before start time.".to_string());
    }
    let sample_count = stop
        .checked_sub(start)
        .and_then(|duration| duration.checked_div(sample_time))
        .and_then(|count| count.checked_add(1))
        .and_then(|count| usize::try_from(count).ok())
        .ok_or_else(|| "The selected range contains too many samples.".to_string())?;
    let fft_length = sample_count
        .checked_add(state.zero_padding)
        .ok_or_else(|| "FFT length is too large.".to_string())?;
    if sample_count == 0 || fft_length > MAX_FFT_SAMPLES {
        return Err(format!(
            "FFT length must be between 1 and {MAX_FFT_SAMPLES} samples."
        ));
    }

    let Some(real_signal) = state.real_signal else {
        return Err("Select a real signal.".to_string());
    };
    let real_samples = signal_samples(
        waves,
        translators,
        real_signal,
        start,
        sample_time,
        sample_count,
    )?;
    let imaginary_samples = match state.imaginary_signal {
        Some(imaginary_signal) => Some(signal_samples(
            waves,
            translators,
            imaginary_signal,
            start,
            sample_time,
            sample_count,
        )?),
        None => None,
    };

    let sample_seconds = sample_time as f64 * timescale.seconds_per_tick();
    if !sample_seconds.is_finite() || sample_seconds <= 0.0 {
        return Err("Sample time is outside the supported frequency range.".to_string());
    }

    let mut buffer = vec![Complex::new(0.0, 0.0); fft_length];
    for index in 0..sample_count {
        let imaginary = imaginary_samples
            .as_ref()
            .map_or(0.0, |samples| samples[index]);
        buffer[index] = Complex::new(real_samples[index], imaginary);
    }

    let mut planner = FftPlanner::<f64>::new();
    planner.plan_fft_forward(fft_length).process(&mut buffer);

    let sample_times = (0..sample_count)
        .map(|index| index as f64 * sample_seconds)
        .collect();
    let positive_bins = fft_length / 2 + 1;
    let component_scale = 1.0 / fft_length as f64;
    let mut fft_real = Vec::with_capacity(positive_bins);
    let mut fft_imaginary = Vec::with_capacity(positive_bins);
    let mut magnitude = Vec::with_capacity(positive_bins);
    let mut phase = Vec::with_capacity(positive_bins);
    for (index, value) in buffer.into_iter().take(positive_bins).enumerate() {
        let frequency = index as f64 / (fft_length as f64 * sample_seconds);
        let one_sided_scale = if index > 0 && index * 2 < fft_length {
            2.0
        } else {
            1.0
        };
        fft_real.push([frequency, value.re * component_scale * one_sided_scale]);
        fft_imaginary.push([frequency, value.im * component_scale * one_sided_scale]);
        let mut value_magnitude = value.norm() * component_scale;
        if index > 0 && index * 2 < fft_length {
            value_magnitude *= 2.0;
        }
        magnitude.push([frequency, value_magnitude]);
        phase.push([frequency, value.im.atan2(value.re)]);
    }

    Ok(Spectrum {
        sample_times,
        real_samples,
        imaginary_samples,
        fft_real,
        fft_imaginary,
        magnitude,
        phase,
        sample_count,
        fft_length,
    })
}

fn signal_samples(
    waves: &WaveData,
    translators: &TranslatorList,
    item_ref: DisplayedItemRef,
    start: u64,
    sample_time: u64,
    sample_count: usize,
) -> Result<Vec<f64>, String> {
    let Some(DisplayedItem::Variable(variable)) = waves.displayed_items.get(&item_ref) else {
        return Err("The selected item is not a signal.".to_string());
    };
    let wave_container = waves
        .inner
        .as_waves()
        .ok_or_else(|| "FFT is only available for waveform signals.".to_string())?;
    let meta = wave_container
        .variable_meta(&variable.variable_ref)
        .map_err(|error| format!("Could not read signal metadata: {error}"))?;
    let field = DisplayedFieldRef::from(item_ref);
    let translator = waves.variable_translator(&field, translators);
    let translator_name = translator.name();
    let mut values = Vec::with_capacity(sample_count);

    for index in 0..sample_count {
        let time = start
            .checked_add(sample_time.checked_mul(index as u64).ok_or_else(|| {
                "The selected sample range is outside the supported time range.".to_string()
            })?)
            .ok_or_else(|| {
                "The selected sample range is outside the supported time range.".to_string()
            })?;
        let value = wave_container
            .query_variable(&variable.variable_ref, &BigUint::from(time))
            .map_err(|error| format!("Could not read signal value: {error}"))?
            .and_then(|result| result.current.map(|(_, value)| value));

        let numeric = match value {
            Some(value) => translator.translate_numeric(&meta, &value).ok_or_else(|| {
                format!("Translator '{translator_name}' has no numeric value for this signal.")
            })?,
            None => 0.0,
        };
        values.push(numeric);
    }

    Ok(values)
}
