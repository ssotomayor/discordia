use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraQuality {
    pub height: u32,
    pub fps: u32,
}

impl Default for CameraQuality {
    fn default() -> Self {
        Self {
            height: 720,
            fps: 30,
        }
    }
}

impl CameraQuality {
    pub fn normalized(self) -> Self {
        Self {
            height: match self.height {
                480 | 720 | 1080 => self.height,
                _ => 720,
            },
            fps: match self.fps {
                15 | 30 | 60 => self.fps,
                _ => 30,
            },
        }
    }

    pub fn width(self) -> u32 {
        match self.normalized().height {
            480 => 854,
            1080 => 1920,
            _ => 1280,
        }
    }

    pub fn bitrate(self) -> u32 {
        let quality = self.normalized();
        let baseline = match quality.height {
            480 => 600_000,
            1080 => 3_000_000,
            _ => 1_200_000,
        };
        baseline * quality.fps / 30
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_invalid_values_cannot_request_unbounded_capture() {
        let quality = CameraQuality {
            height: u32::MAX,
            fps: u32::MAX,
        }
        .normalized();
        assert_eq!(quality, CameraQuality::default());
        assert_eq!(quality.bitrate(), 1_200_000);
    }

    #[test]
    fn automatic_budget_tracks_resolution_and_frame_rate() {
        assert_eq!(
            CameraQuality {
                height: 480,
                fps: 15
            }
            .bitrate(),
            300_000
        );
        assert_eq!(
            CameraQuality {
                height: 1080,
                fps: 60
            }
            .bitrate(),
            6_000_000
        );
        assert_eq!(
            serde_json::from_str::<CameraQuality>("{}").unwrap(),
            CameraQuality::default()
        );
    }
}
