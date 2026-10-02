use crate::protocol::rendezvous::GeoPoint;

const LAND: &[u8; 8100] = include_bytes!("../../assets/globe-land.bin");

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub(super) struct Point {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(serde::Serialize)]
pub(super) struct Focus {
    yaw: f64,
    pitch: f64,
}

pub(super) fn coordinates(point: GeoPoint) -> Option<(Point, Focus)> {
    point.coarse()?;
    let lat = point.lat.to_radians();
    let lon = point.lon.to_radians();
    Some((
        Point {
            x: lat.cos() * lon.cos(),
            y: lat.sin(),
            z: -lat.cos() * lon.sin(),
        },
        Focus {
            yaw: -std::f64::consts::FRAC_PI_2 - lon,
            pitch: lat,
        },
    ))
}

pub(super) fn dots(radius: f64) -> Vec<Point> {
    if !radius.is_finite() || radius <= 0.0 {
        return Vec::new();
    }
    let count = (radius * radius * 0.3).round().clamp(1500.0, 9000.0) as usize;
    let golden = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    (0..count)
        .filter_map(|i| {
            let y = 1.0 - (2 * i + 1) as f64 / count as f64;
            let radial = (1.0 - y * y).sqrt();
            let angle = golden * i as f64;
            let point = Point {
                x: angle.cos() * radial,
                y,
                z: angle.sin() * radial,
            };
            let lat = y.asin().to_degrees();
            let lon = (-point.z).atan2(point.x).to_degrees();
            let row = (90.0 - lat).floor().clamp(0.0, 179.0) as usize;
            let col = (lon + 180.0).floor().clamp(0.0, 359.0) as usize;
            let bit = row * 360 + col;
            ((LAND[bit / 8] >> (bit % 8)) & 1 != 0).then_some(point)
        })
        .collect()
}

#[derive(serde::Deserialize)]
pub(super) struct PlaceRequest {
    sx: f64,
    sy: f64,
    cx: f64,
    cy: f64,
    radius: f64,
    yaw: f64,
    pitch: f64,
}

impl PlaceRequest {
    pub fn point(&self) -> Option<GeoPoint> {
        if self.radius <= 0.0
            || ![
                self.sx,
                self.sy,
                self.cx,
                self.cy,
                self.radius,
                self.yaw,
                self.pitch,
            ]
            .into_iter()
            .all(f64::is_finite)
        {
            return None;
        }
        let x = (self.sx - self.cx) / self.radius;
        let y = -(self.sy - self.cy) / self.radius;
        let d = x * x + y * y;
        if d > 1.0 {
            return None;
        }
        let z = (1.0 - d).sqrt();
        let y0 = y * self.pitch.cos() + z * self.pitch.sin();
        let z1 = -y * self.pitch.sin() + z * self.pitch.cos();
        let x0 = x * self.yaw.cos() - z1 * self.yaw.sin();
        let z0 = x * self.yaw.sin() + z1 * self.yaw.cos();
        (GeoPoint {
            lat: y0.clamp(-1.0, 1.0).asin().to_degrees(),
            lon: (-z0).atan2(x0).to_degrees(),
        })
        .coarse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geographic_coordinates_round_trip_through_the_visible_hemisphere() {
        for geo in [
            GeoPoint { lat: 0.0, lon: 0.0 },
            GeoPoint {
                lat: -33.9,
                lon: 151.2,
            },
            GeoPoint {
                lat: 89.0,
                lon: -170.0,
            },
        ] {
            let (p, focus) = coordinates(geo).unwrap();
            let x = p.x * focus.yaw.cos() + p.z * focus.yaw.sin();
            let z = -p.x * focus.yaw.sin() + p.z * focus.yaw.cos();
            let y = p.y * focus.pitch.cos() - z * focus.pitch.sin();
            let request = PlaceRequest {
                sx: 100.0 + 90.0 * x,
                sy: 100.0 - 90.0 * y,
                cx: 100.0,
                cy: 100.0,
                radius: 90.0,
                yaw: focus.yaw,
                pitch: focus.pitch,
            };
            assert_eq!(request.point(), geo.coarse());
        }
    }
    #[test]
    fn land_points_are_bounded_and_on_the_unit_sphere() {
        // Counts preserve the original Canvas globe's Natural Earth mask and sampling.
        for (radius, count) in [(20.0, 429), (130.0, 1472), (1000000.0, 2588)] {
            let points = dots(radius);
            assert_eq!(points.len(), count);
            assert!(!points.is_empty() && points.len() <= 9000);
            for p in points {
                assert!((p.x * p.x + p.y * p.y + p.z * p.z - 1.0).abs() < 1e-12);
            }
        }
        assert!(dots(f64::NAN).is_empty());
        assert!(
            coordinates(GeoPoint {
                lat: 91.0,
                lon: 0.0
            })
            .is_none()
        );
        let mut request = PlaceRequest {
            sx: 101.0,
            sy: 0.0,
            cx: 0.0,
            cy: 0.0,
            radius: 100.0,
            yaw: 0.0,
            pitch: 0.0,
        };
        assert!(request.point().is_none());
        request.sx = 0.0;
        request.radius = 0.0;
        assert!(request.point().is_none());
    }
}
