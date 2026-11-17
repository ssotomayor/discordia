use std::mem::ManuallyDrop;
use std::time::{Duration, Instant};

use livekit::webrtc::video_frame::I420Buffer;
use windows_capture_api::Win32::Foundation::{E_FAIL, RECT};
use windows_capture_api::Win32::Graphics::Direct3D11::*;
use windows_capture_api::Win32::Graphics::Dxgi::Common::*;
use windows_capture_api::core::{Error, Interface, Result};

#[derive(Default)]
pub(super) struct Choice {
    pub selected: Option<bool>,
    samples: u32,
    cpu: [Duration; 6],
    gpu: [Duration; 6],
    cpu_samples: usize,
    gpu_samples: usize,
    late: u8,
    recent: u8,
    last_finish: Option<Instant>,
}

impl Choice {
    pub fn begin(&mut self, now: Instant) -> bool {
        let idle = self.last_finish.is_some_and(|previous| {
            now.saturating_duration_since(previous) > Duration::from_millis(500)
        });
        if idle {
            *self = Self::default();
        }
        idle
    }

    pub fn finish(&mut self, now: Instant) {
        self.last_finish = Some(now);
    }
    pub fn use_gpu(&self) -> bool {
        self.selected.unwrap_or(matches!(self.samples % 4, 1 | 2))
    }
    pub fn observe(&mut self, elapsed: Duration, fps: u32) -> Option<bool> {
        let gpu = self.use_gpu();
        if self.selected.is_some() {
            return self.selected;
        }
        if gpu && elapsed > Duration::from_secs_f64(1.0 / f64::from(fps.max(1))) {
            self.selected = Some(false);
            return self.selected;
        }
        if self.samples >= 4 {
            if gpu {
                self.gpu[self.gpu_samples] = elapsed;
                self.gpu_samples += 1;
            } else {
                self.cpu[self.cpu_samples] = elapsed;
                self.cpu_samples += 1;
            }
        }
        self.samples += 1;
        if self.samples == 16 {
            let Some((cpu, gpu)) = self.costs() else {
                self.selected = Some(false);
                return self.selected;
            };
            let budget = Duration::from_secs_f64(0.75 / f64::from(fps.max(1)));
            self.selected = Some(gpu.as_secs_f64() < cpu.as_secs_f64() * 0.9 && gpu < budget);
        }
        self.selected
    }

    pub fn over_budget(&mut self, elapsed: Duration, fps: u32) -> bool {
        self.late = (self.late << 1)
            | u8::from(elapsed > Duration::from_secs_f64(1.0 / f64::from(fps.max(1))));
        self.recent = self.recent.saturating_add(1).min(8);
        self.recent == 8 && self.late.count_ones() >= 3
    }

    pub fn costs(&self) -> Option<(Duration, Duration)> {
        if self.cpu_samples != 6 || self.gpu_samples != 6 {
            return None;
        }
        let percentile = |mut samples: [Duration; 6]| {
            samples.sort_unstable();
            samples[4]
        };
        Some((percentile(self.cpu), percentile(self.gpu)))
    }
}

pub(super) struct Readback {
    context: ID3D11DeviceContext,
    staging: ID3D11Texture2D,
    geometry: (u32, u32),
    device: ID3D11Device,
}

impl Readback {
    pub fn matches(&self, device: &ID3D11Device, width: u32, height: u32) -> bool {
        self.device == *device && self.geometry == (width, height)
    }

    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let mut staging = None;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_STAGING,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            ..Default::default()
        };
        // SAFETY: The descriptor creates an owned, single-subresource CPU-readable texture.
        unsafe {
            device.CreateTexture2D(&desc, None, Some(&mut staging))?;
        }
        Ok(Self {
            context: context.clone(),
            device: device.clone(),
            staging: staging.ok_or_else(|| Error::from(E_FAIL))?,
            geometry: (width, height),
        })
    }

    pub fn map(&mut self, texture: &ID3D11Texture2D) -> Result<Mapped<'_>> {
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: Capture owns the matching source through readback; the guard unmaps before reuse.
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            if self.geometry != (desc.Width, desc.Height)
                || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
                || desc.ArraySize != 1
                || desc.MipLevels != 1
                || desc.SampleDesc.Count != 1
                || texture.GetDevice()? != self.device
            {
                return Err(Error::from(E_FAIL));
            }
            self.context.CopyResource(&self.staging, texture);
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
        }
        Ok(Mapped {
            context: &self.context,
            texture: &self.staging,
            data: mapped,
        })
    }
}

pub(super) struct Processor {
    device: ID3D11VideoDevice,
    context: ID3D11DeviceContext,
    video_context: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    output: ID3D11Texture2D,
    output_view: ID3D11VideoProcessorOutputView,
    staging: ID3D11Texture2D,
    pub geometry: (u32, u32, u32, u32),
}

impl Processor {
    pub fn new(
        device: &ID3D11Device,
        context: &ID3D11DeviceContext,
        geometry: (u32, u32, u32, u32),
        fps: u32,
    ) -> Result<Self> {
        let (input_width, input_height, width, height) = geometry;
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(Error::from(E_FAIL));
        }
        let video_device: ID3D11VideoDevice = device.cast()?;
        let video_context: ID3D11VideoContext = context.cast()?;
        let color_context: ID3D11VideoContext1 = context.cast()?;
        let rate = DXGI_RATIONAL {
            Numerator: fps.max(1),
            Denominator: 1,
        };
        let content = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: rate,
            InputWidth: input_width,
            InputHeight: input_height,
            OutputFrameRate: rate,
            OutputWidth: width,
            OutputHeight: height,
            Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
        };
        // All surfaces stay owned on the capture thread through the synchronous readback.
        unsafe {
            let enumerator = video_device.CreateVideoProcessorEnumerator(&content)?;
            let support = enumerator.CheckVideoProcessorFormat(DXGI_FORMAT_NV12)?;
            if support & D3D11_VIDEO_PROCESSOR_FORMAT_SUPPORT_OUTPUT.0 as u32 == 0 {
                return Err(Error::from(E_FAIL));
            }
            let formats: ID3D11VideoProcessorEnumerator1 = enumerator.cast()?;
            if !formats
                .CheckVideoProcessorFormatConversion(
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                    DXGI_FORMAT_NV12,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
                )?
                .as_bool()
            {
                return Err(Error::from(E_FAIL));
            }
            let processor = video_device.CreateVideoProcessor(&enumerator, 0)?;
            let mut output = None;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            device.CreateTexture2D(&desc, None, Some(&mut output))?;
            let output = output.ok_or_else(|| Error::from(E_FAIL))?;
            let mut staging = None;
            device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..desc
                },
                None,
                Some(&mut staging),
            )?;
            let staging = staging.ok_or_else(|| Error::from(E_FAIL))?;
            let mut output_view = None;
            video_device.CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut output_view),
            )?;
            let output_view = output_view.ok_or_else(|| Error::from(E_FAIL))?;
            video_context.VideoProcessorSetStreamFrameFormat(
                &processor,
                0,
                D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            );
            video_context.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            video_context.VideoProcessorSetStreamSourceRect(
                &processor,
                0,
                true,
                Some(&RECT {
                    left: 0,
                    top: 0,
                    right: input_width as i32,
                    bottom: input_height as i32,
                }),
            );
            let destination = RECT {
                left: 0,
                top: 0,
                right: width as i32,
                bottom: height as i32,
            };
            video_context.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&destination));
            video_context.VideoProcessorSetOutputTargetRect(&processor, true, Some(&destination));
            // Match the limited-range BT.601 used by the existing libyuv conversion.
            color_context.VideoProcessorSetStreamColorSpace1(
                &processor,
                0,
                DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
            );
            color_context.VideoProcessorSetOutputColorSpace1(
                &processor,
                DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P601,
            );
            Ok(Self {
                device: video_device,
                context: context.clone(),
                video_context,
                enumerator,
                processor,
                output,
                output_view,
                staging,
                geometry,
            })
        }
    }

    pub fn convert(
        &mut self,
        texture: &ID3D11Texture2D,
    ) -> Result<(I420Buffer, Duration, Duration)> {
        let (_, _, width, height) = self.geometry;
        unsafe {
            let mut input_view = None;
            self.device.CreateVideoProcessorInputView(
                texture,
                &self.enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut input_view),
            )?;
            let input_view = input_view.ok_or_else(|| Error::from(E_FAIL))?;
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ..Default::default()
            };
            let result = self.video_context.VideoProcessorBlt(
                &self.processor,
                &self.output_view,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
            let readback_started = Instant::now();
            self.context.CopyResource(&self.staging, &self.output);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let mapped = Mapped {
                context: &self.context,
                texture: &self.staging,
                data: mapped,
            };
            let readback = readback_started.elapsed();
            let conversion_started = Instant::now();
            if mapped.data.pData.is_null() || mapped.data.RowPitch < width {
                return Err(Error::from(E_FAIL));
            }
            let y_len = mapped.data.RowPitch as usize * height as usize;
            // The owned NV12 texture has height Y rows followed by height/2 UV rows at this pitch.
            let pixels =
                std::slice::from_raw_parts(mapped.data.pData.cast::<u8>(), y_len + y_len / 2);
            let mut buffer = I420Buffer::new(width, height);
            let (sy, su, sv) = buffer.strides();
            let (y, u, v) = buffer.data_mut();
            livekit::webrtc::native::yuv_helper::nv12_to_i420(
                &pixels[..y_len],
                mapped.data.RowPitch,
                &pixels[y_len..],
                mapped.data.RowPitch,
                y,
                sy,
                u,
                su,
                v,
                sv,
                width as i32,
                height as i32,
            );
            Ok((buffer, readback, conversion_started.elapsed()))
        }
    }
}

pub(super) struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    data: D3D11_MAPPED_SUBRESOURCE,
}

impl Mapped<'_> {
    pub fn row_pitch(&self) -> u32 {
        self.data.RowPitch
    }

    pub fn bgra(&self, width: u32, height: u32) -> Result<&[u8]> {
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: The guard owns a live mapping and borrows the backing texture.
        unsafe {
            self.texture.GetDesc(&mut desc);
        }
        if (width, height) != (desc.Width, desc.Height) || desc.Format != DXGI_FORMAT_B8G8R8A8_UNORM
        {
            return Err(Error::from(E_FAIL));
        }
        if self.data.pData.is_null() || self.data.RowPitch < width.saturating_mul(4) {
            return Err(Error::from(E_FAIL));
        }
        let len = (self.data.RowPitch as usize)
            .checked_mul(height as usize)
            .ok_or_else(|| Error::from(E_FAIL))?;
        // SAFETY: The mapped owned staging texture contains height rows of its reported row pitch.
        Ok(unsafe { std::slice::from_raw_parts(self.data.pData.cast::<u8>(), len) })
    }
}

impl Drop for Mapped<'_> {
    fn drop(&mut self) {
        unsafe {
            self.context.Unmap(self.texture, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use livekit::webrtc::video_frame::VideoBuffer;
    use windows_capture_api::Win32::Foundation::HMODULE;
    use windows_capture_api::Win32::Graphics::Direct3D::{
        D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP,
    };

    #[test]
    fn cached_readback_preserves_new_pixels_padding_and_mapping_lifetimes() {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let device = device.unwrap();
            let context = context.unwrap();
            for (width, height) in [(64, 32), (65, 33)] {
                let mut readback = Readback::new(&device, &context, width, height).unwrap();
                let mut texture = None;
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: width,
                            Height: height,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                            ..Default::default()
                        },
                        None,
                        Some(&mut texture),
                    )
                    .unwrap();
                let texture = texture.unwrap();
                assert!(readback.matches(&device, width, height));
                assert!(!readback.matches(&device, width + 1, height));
                let mut mismatch = Readback::new(&device, &context, width + 1, height).unwrap();
                assert!(mismatch.map(&texture).is_err());
                for frame in 1u8..=4 {
                    let pixel = [frame, 20, 30, 255];
                    let pixels = pixel.repeat(width as usize * height as usize);
                    context.UpdateSubresource(
                        &texture,
                        0,
                        None,
                        pixels.as_ptr().cast(),
                        width * 4,
                        0,
                    );
                    let mapped = readback.map(&texture).unwrap();
                    let data = mapped.bgra(width, height).unwrap();
                    for row in 0..height {
                        let offset = (row * mapped.row_pitch() + (width - 1) * 4) as usize;
                        assert_eq!(&data[offset..offset + 4], pixel);
                    }
                    assert!(mapped.bgra(width, height + 1).is_err());
                    drop(mapped);
                }
            }
        }
    }

    #[test]
    fn preprocessing_selection_requires_a_measured_gain_and_respects_the_frame_budget() {
        for (cpu_ms, gpu_ms, fps, wanted) in [
            (2, 4, 60, false),
            (5, 1, 60, true),
            (16, 13, 60, false),
            (30, 13, 30, true),
        ] {
            let mut choice = Choice::default();
            for frame in 0..16 {
                assert_eq!(choice.use_gpu(), matches!(frame % 4, 1 | 2));
                let elapsed = Duration::from_millis(if choice.use_gpu() { gpu_ms } else { cpu_ms });
                assert_eq!(
                    choice.observe(elapsed, fps),
                    (frame == 15).then_some(wanted)
                );
            }
        }
        let mut choice = Choice::default();
        for frame in 0..8 {
            assert_eq!(
                choice.over_budget(
                    Duration::from_millis(if frame % 3 == 0 { 20 } else { 3 }),
                    60
                ),
                frame == 7
            );
        }
        let mut overloaded = Choice::default();
        assert_eq!(overloaded.observe(Duration::from_millis(2), 60), None);
        assert_eq!(
            overloaded.observe(Duration::from_millis(50), 60),
            Some(false)
        );
        assert!(overloaded.costs().is_none());
    }

    #[test]
    fn isolated_cpu_spikes_do_not_make_a_slower_gpu_look_faster() {
        let mut choice = Choice::default();
        for frame in 0..16 {
            let elapsed = if frame == 4 {
                100
            } else if choice.use_gpu() {
                3
            } else {
                2
            };
            choice.observe(Duration::from_millis(elapsed), 60);
        }
        assert_eq!(choice.selected, Some(false));
        assert_eq!(
            choice.costs(),
            Some((Duration::from_millis(2), Duration::from_millis(3)))
        );
    }

    #[test]
    fn an_idle_interval_restarts_measurement_from_cpu_instead_of_reusing_old_load_samples() {
        let start = Instant::now();
        let mut choice = Choice::default();
        for frame in 0..16 {
            choice.begin(start + Duration::from_millis(frame * 16));
            let cost = Duration::from_millis(if choice.use_gpu() { 1 } else { 3 });
            choice.observe(cost, 60);
            choice.finish(start + Duration::from_millis(frame * 16) + cost);
        }
        assert_eq!(choice.selected, Some(true));
        choice.begin(start + Duration::from_secs(2));
        assert_eq!(choice.selected, None);
        assert!(!choice.use_gpu());
        assert!(choice.costs().is_none());
    }

    #[test]
    fn a_slow_readback_is_not_mistaken_for_idle_and_cannot_trigger_endless_gpu_probes() {
        let start = Instant::now();
        let mut choice = Choice::default();
        choice.begin(start);
        choice.observe(Duration::from_millis(2), 60);
        choice.finish(start + Duration::from_millis(2));
        choice.begin(start + Duration::from_millis(16));
        assert_eq!(choice.observe(Duration::from_secs(1), 60), Some(false));
        choice.finish(start + Duration::from_millis(1016));
        assert!(!choice.begin(start + Duration::from_millis(1017)));
        assert_eq!(choice.selected, Some(false));
    }

    #[test]
    #[ignore = "requires a Windows GPU; compares the two capture preparation paths"]
    fn gpu_preprocessing_benchmark_1440p() {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let device = device.unwrap();
            let context = context.unwrap();
            let mut texture = None;
            let source_desc = D3D11_TEXTURE2D_DESC {
                Width: 2560,
                Height: 1440,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                ..Default::default()
            };
            let pixels = [50u8, 100, 150, 255].repeat(2560 * 1440);
            device
                .CreateTexture2D(
                    &source_desc,
                    Some(&D3D11_SUBRESOURCE_DATA {
                        pSysMem: pixels.as_ptr().cast(),
                        SysMemPitch: 2560 * 4,
                        SysMemSlicePitch: 0,
                    }),
                    Some(&mut texture),
                )
                .unwrap();
            let texture = texture.unwrap();
            for (width, height) in [(2560, 1440), (1920, 1080)] {
                let mut gpu =
                    Processor::new(&device, &context, (2560, 1440, width, height), 60).unwrap();
                let settings = super::super::Settings {
                    width,
                    height,
                    fps: 60,
                    max_bitrate: 34000000,
                    adaptive_quality: false,
                    priority: crate::sysvideo::Priority::Motion,
                    codec: crate::sysvideo::Codec::H264,
                    encoder: crate::sysvideo::Encoder::Auto,
                    gpu_preprocessing: true,
                };
                let mut cpu = super::super::BgraConverter::default();
                let mut cached = Readback::new(&device, &context, 2560, 1440).unwrap();
                let mut times: [Vec<f64>; 3] = Default::default();
                for iteration in 0..108 {
                    let order = [0, 1, 2, 2, 1, 0];
                    let path = order[iteration % order.len()];
                    let began = Instant::now();
                    if path == 2 {
                        std::hint::black_box(gpu.convert(&texture).unwrap());
                    } else if path == 1 {
                        let mapped = cached.map(&texture).unwrap();
                        std::hint::black_box(
                            cpu.convert(
                                mapped.bgra(2560, 1440).unwrap(),
                                mapped.row_pitch(),
                                2560,
                                1440,
                                settings,
                            )
                            .unwrap(),
                        );
                    } else {
                        let mut staging = None;
                        device
                            .CreateTexture2D(
                                &D3D11_TEXTURE2D_DESC {
                                    Usage: D3D11_USAGE_STAGING,
                                    BindFlags: 0,
                                    CPUAccessFlags: (D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE)
                                        .0
                                        as u32,
                                    ..source_desc
                                },
                                None,
                                Some(&mut staging),
                            )
                            .unwrap();
                        let staging = staging.unwrap();
                        context.CopyResource(&staging, &texture);
                        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                        context
                            .Map(&staging, 0, D3D11_MAP_READ_WRITE, 0, Some(&mut mapped))
                            .unwrap();
                        let mapped = Mapped {
                            context: &context,
                            texture: &staging,
                            data: mapped,
                        };
                        std::hint::black_box(
                            cpu.convert(
                                mapped.bgra(2560, 1440).unwrap(),
                                mapped.row_pitch(),
                                2560,
                                1440,
                                settings,
                            )
                            .unwrap(),
                        );
                    }
                    if iteration >= 12 {
                        times[path].push(began.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                for (path, mut samples) in times.into_iter().enumerate() {
                    samples.sort_by(f64::total_cmp);
                    let mean = samples.iter().sum::<f64>() / samples.len() as f64;
                    let p75 = samples[(samples.len() * 3).div_ceil(4) - 1];
                    eprintln!(
                        "capture preparation 2560x1440 -> {width}x{height} {}: mean {mean:.3} ms/frame, p75 {p75:.3} (32 samples, fresh allocation included for legacy)",
                        ["legacy CPU", "cached CPU", "GPU NV12"][path]
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "requires a Windows GPU with a D3D11 video processor"]
    fn gpu_preprocessing_preserves_color_range_and_resizes_without_retaining_input() {
        unsafe {
            let mut device = None;
            let mut context = None;
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            )
            .unwrap();
            let device = device.unwrap();
            let context = context.unwrap();
            let mut processor = Processor::new(&device, &context, (64, 32, 32, 16), 60).unwrap();
            for (bgra, expected) in [
                ([0u8, 0, 0, 255], [16, 128, 128]),
                ([255, 255, 255, 255], [235, 128, 128]),
                ([0, 0, 255, 255], [82, 90, 240]),
                ([0, 255, 0, 255], [144, 54, 34]),
                ([255, 0, 0, 255], [41, 240, 110]),
            ] {
                let pixels = bgra.repeat(64 * 32);
                let mut texture = None;
                device
                    .CreateTexture2D(
                        &D3D11_TEXTURE2D_DESC {
                            Width: 64,
                            Height: 32,
                            MipLevels: 1,
                            ArraySize: 1,
                            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            SampleDesc: DXGI_SAMPLE_DESC {
                                Count: 1,
                                Quality: 0,
                            },
                            Usage: D3D11_USAGE_DEFAULT,
                            BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                            ..Default::default()
                        },
                        Some(&D3D11_SUBRESOURCE_DATA {
                            pSysMem: pixels.as_ptr().cast(),
                            SysMemPitch: 64 * 4,
                            SysMemSlicePitch: 0,
                        }),
                        Some(&mut texture),
                    )
                    .unwrap();
                let texture = texture.unwrap();
                let (buffer, _, _) = processor.convert(&texture).unwrap();
                drop(texture);
                assert_eq!((buffer.width(), buffer.height()), (32, 16));
                let (y, u, v) = buffer.data();
                for (plane, expected) in [y, u, v].into_iter().zip(expected) {
                    assert!(
                        (i32::from(plane[0]) - expected).abs() <= 4,
                        "BGRA {bgra:?}: {} differs from {expected}",
                        plane[0]
                    );
                }
            }
        }
    }
}
