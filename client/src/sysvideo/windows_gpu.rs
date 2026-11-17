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
    cpu: Duration,
    gpu: Duration,
    late: u8,
}

impl Choice {
    pub fn compare_cpu_first(&self) -> bool {
        self.selected.is_none() && !self.samples.is_multiple_of(2)
    }
    pub fn observe(&mut self, cpu: Duration, gpu: Duration, fps: u32) -> Option<bool> {
        self.samples += 1;
        if self.samples > 2 {
            self.cpu += cpu;
            self.gpu += gpu;
        }
        if self.samples == 6 {
            let budget = Duration::from_secs_f64(0.75 / f64::from(fps.max(1)));
            self.selected = Some(
                self.gpu.as_secs_f64() < self.cpu.as_secs_f64() * 0.9 && self.gpu / 4 < budget,
            );
        }
        self.selected
    }

    pub fn over_budget(&mut self, elapsed: Duration, fps: u32) -> bool {
        if elapsed > Duration::from_secs_f64(1.0 / f64::from(fps.max(1))) {
            self.late = self.late.saturating_add(1);
        } else {
            self.late = 0;
        }
        self.late >= 8
    }

    pub fn costs(&self) -> (Duration, Duration) {
        (self.cpu / 4, self.gpu / 4)
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

    pub fn convert(&self, texture: &ID3D11Texture2D) -> Result<(I420Buffer, Duration, Duration)> {
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

struct Mapped<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
    data: D3D11_MAPPED_SUBRESOURCE,
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
    use windows_capture_api::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;

    #[test]
    fn preprocessing_selection_requires_a_measured_gain_and_respects_the_frame_budget() {
        for (cpu_ms, gpu_ms, fps, wanted) in [
            (2, 4, 60, false),
            (5, 1, 60, true),
            (16, 13, 60, false),
            (30, 13, 30, true),
        ] {
            let mut choice = Choice::default();
            for _ in 0..2 {
                assert_eq!(
                    choice.observe(Duration::from_secs(1), Duration::from_secs(1), fps),
                    None
                );
            }
            for i in 0..4 {
                assert_eq!(
                    choice.observe(
                        Duration::from_millis(cpu_ms),
                        Duration::from_millis(gpu_ms),
                        fps
                    ),
                    (i == 3).then_some(wanted)
                );
            }
        }
        let mut choice = Choice::default();
        for _ in 0..7 {
            assert!(!choice.over_budget(Duration::from_millis(20), 60));
        }
        assert!(!choice.over_budget(Duration::from_millis(3), 60));
        for _ in 0..7 {
            assert!(!choice.over_budget(Duration::from_millis(20), 60));
        }
        assert!(choice.over_budget(Duration::from_millis(20), 60));
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
            let mut staging = None;
            device
                .CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Usage: D3D11_USAGE_STAGING,
                        BindFlags: 0,
                        CPUAccessFlags: (D3D11_CPU_ACCESS_READ | D3D11_CPU_ACCESS_WRITE).0 as u32,
                        ..source_desc
                    },
                    None,
                    Some(&mut staging),
                )
                .unwrap();
            let staging = staging.unwrap();
            for (width, height) in [(2560, 1440), (1920, 1080)] {
                let gpu =
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
                };
                let mut cpu = super::super::BgraConverter::default();
                let mut gpu_times = Vec::new();
                let mut cpu_times = Vec::new();
                for iteration in 0..25 {
                    let began = Instant::now();
                    let (buffer, _, _) = gpu.convert(&texture).unwrap();
                    std::hint::black_box(buffer);
                    let gpu_ms = began.elapsed().as_secs_f64() * 1000.0;
                    let began = Instant::now();
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
                    let pixels = std::slice::from_raw_parts_mut(
                        mapped.data.pData.cast::<u8>(),
                        mapped.data.RowPitch as usize * 1440,
                    );
                    std::hint::black_box(
                        cpu.convert(pixels, mapped.data.RowPitch, 2560, 1440, settings)
                            .unwrap(),
                    );
                    drop(mapped);
                    let cpu_ms = began.elapsed().as_secs_f64() * 1000.0;
                    if iteration >= 5 {
                        gpu_times.push(gpu_ms);
                        cpu_times.push(cpu_ms);
                    }
                }
                let mean = |values: &[f64]| values.iter().sum::<f64>() / values.len() as f64;
                eprintln!(
                    "capture preparation 2560x1440 -> {width}x{height}: GPU {:.3} ms/frame, CPU {:.3} ms/frame (20 frames, synchronous readback included)",
                    mean(&gpu_times),
                    mean(&cpu_times)
                );
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
            let processor = Processor::new(&device, &context, (64, 32, 32, 16), 60).unwrap();
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
