#[cxx::bridge(namespace = "livekit_ffi")]
pub mod ffi {
    pub struct CameraDevice {
        pub id: String,
        pub label: String,
    }

    extern "Rust" {
        type CameraSinkWrapper;
        fn on_frame(
            self: &CameraSinkWrapper,
            width: u32,
            height: u32,
            stride_y: u32,
            stride_u: u32,
            stride_v: u32,
            y: &[u8],
            u: &[u8],
            v: &[u8],
            timestamp_us: i64,
        );
    }

    unsafe extern "C++" {
        include!("livekit/camera_capture.h");
        type CameraCapture;
        fn camera_devices() -> Vec<CameraDevice>;
        fn start_camera_capture(
            id: &str,
            width: u32,
            height: u32,
            fps: u32,
            sink: Box<CameraSinkWrapper>,
        ) -> Result<UniquePtr<CameraCapture>>;
    }
}

pub trait CameraSink: Send + Sync {
    fn on_frame(
        &self,
        width: u32,
        height: u32,
        strides: (u32, u32, u32),
        planes: (&[u8], &[u8], &[u8]),
        timestamp_us: i64,
    );
}

pub struct CameraSinkWrapper {
    observer: Box<dyn CameraSink>,
}

impl CameraSinkWrapper {
    pub fn new(observer: Box<dyn CameraSink>) -> Self {
        Self { observer }
    }
    #[allow(clippy::too_many_arguments)]
    fn on_frame(
        &self,
        width: u32,
        height: u32,
        stride_y: u32,
        stride_u: u32,
        stride_v: u32,
        y: &[u8],
        u: &[u8],
        v: &[u8],
        timestamp_us: i64,
    ) {
        self.observer.on_frame(
            width,
            height,
            (stride_y, stride_u, stride_v),
            (y, u, v),
            timestamp_us,
        );
    }
}
