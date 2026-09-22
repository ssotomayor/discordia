//! The machine's timezone as a spot on the globe: the zone's reference city
//! from tzdata's `zone.tab`. A guess the host corrects, never a fact (trap 25).

use crate::protocol::rendezvous::GeoPoint;

const ZONE_TAB: &str = include_str!("zone.tab");

pub fn guess() -> Option<GeoPoint> {
    locate(&iana_time_zone::get_timezone().ok()?)
}

pub fn locate(zone: &str) -> Option<GeoPoint> {
    ZONE_TAB
        .lines()
        .filter_map(|l| l.split_once('\t'))
        .find(|(_, name)| *name == zone)
        .and_then(|(coords, _)| iso6709(coords))
        .and_then(GeoPoint::coarse)
}

/// `±DDMM[SS]±DDDMM[SS]`, the one shape zone.tab uses.
fn iso6709(s: &str) -> Option<GeoPoint> {
    let split = s.get(1..)?.find(['+', '-'])? + 1;
    Some(GeoPoint {
        lat: dms(&s[..split])?,
        lon: dms(&s[split..])?,
    })
}

fn dms(s: &str) -> Option<f64> {
    let (sign, digits) = s.split_at_checked(1)?;
    let sign = match sign {
        "+" => 1.0,
        "-" => -1.0,
        _ => return None,
    };
    let deg_len = match digits.len() {
        4 | 6 => 2,
        5 | 7 => 3,
        _ => return None,
    };
    let (d, rest) = digits.split_at(deg_len);
    let (m, sec) = rest.split_at(2);
    let num = |p: &str| -> Option<f64> {
        if p.is_empty() {
            Some(0.0)
        } else {
            p.parse().ok()
        }
    };
    Some(sign * (num(d)? + num(m)? / 60.0 + num(sec)? / 3600.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zone_lands_on_its_reference_city_coarsened() {
        assert_eq!(
            locate("America/St_Kitts"),
            Some(GeoPoint {
                lat: 17.3,
                lon: -62.7
            })
        );
        assert_eq!(
            locate("America/Puerto_Rico"),
            Some(GeoPoint {
                lat: 18.5,
                lon: -66.1
            })
        );
        assert_eq!(
            locate("Asia/Kolkata"),
            Some(GeoPoint {
                lat: 22.5,
                lon: 88.4
            })
        );
        assert_eq!(locate("Etc/UTC"), None);
        assert_eq!(locate(""), None);
    }

    #[test]
    fn minutes_and_seconds_both_parse() {
        let ny = iso6709("+404251-0740023").unwrap();
        assert!((ny.lat - 40.7142).abs() < 0.001);
        assert!((ny.lon + 74.0064).abs() < 0.001);
        let ba = iso6709("-3436-05827").unwrap();
        assert!((ba.lat + 34.6).abs() < 0.001);
        assert!((ba.lon + 58.45).abs() < 0.001);
        assert!(iso6709("+4042").is_none());
        assert!(iso6709("").is_none());
        assert!(iso6709("+40x251-0740023").is_none());
    }

    #[test]
    fn every_row_in_the_table_parses() {
        for (coords, name) in ZONE_TAB.lines().filter_map(|l| l.split_once('\t')) {
            assert!(
                iso6709(coords).and_then(GeoPoint::coarse).is_some(),
                "{name}: {coords}"
            );
        }
    }
}
