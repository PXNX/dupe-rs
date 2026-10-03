//! The photo metadata a backup keeps: when it was taken, where (GPS), and
//! with what camera. Read with `kamadak-exif`; written back with
//! `little_exif`, checked before it replaces the original file.

use exif::{In, Reader, Tag, Value};
use little_exif::exif_tag::ExifTag;
use little_exif::filetype::FileExtension;
use little_exif::metadata::Metadata;
use little_exif::rational::uR64;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufReader, Cursor};
use std::path::Path;

/// Formats EXIF is read from: JPEG, TIFF and TIFF-based raws, HEIF, PNG, WebP.
const READABLE: &[&str] = &[
    "jpg", "jpeg", "jpe", "tif", "tiff", "heic", "heif", "avif", "png", "webp", "dng", "nef",
    "arw", "cr2", "orf", "pef", "srw",
];

/// GPS positions closer than this (in degrees, ~1 m) count as the same.
const GPS_TOLERANCE: f64 = 1e-5;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ExifInfo {
    /// `DateTimeOriginal` as stored, "YYYY:MM:DD HH:MM:SS".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_taken: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gps: Option<Gps>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Gps {
    pub latitude: f64,
    pub longitude: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub altitude: Option<f64>,
}

impl Gps {
    pub fn same_place(&self, other: &Gps) -> bool {
        (self.latitude - other.latitude).abs() < GPS_TOLERANCE
            && (self.longitude - other.longitude).abs() < GPS_TOLERANCE
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_lowercase())
}

/// The file's EXIF date taken, GPS position and camera, if it has any.
pub fn read(path: &Path) -> Option<ExifInfo> {
    if !READABLE.contains(&extension(path)?.as_str()) {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    let exif = Reader::new()
        .read_from_container(&mut BufReader::new(file))
        .ok()?;
    from_exif(&exif)
}

fn from_exif(exif: &exif::Exif) -> Option<ExifInfo> {
    let text = |tag| match &exif.get_field(tag, In::PRIMARY)?.value {
        Value::Ascii(values) => values
            .first()
            .map(|s| {
                String::from_utf8_lossy(s)
                    .trim_end_matches('\0')
                    .trim()
                    .to_owned()
            })
            .filter(|s| !s.is_empty()),
        _ => None,
    };
    let camera = match (text(Tag::Make), text(Tag::Model)) {
        (Some(make), Some(model)) if model.starts_with(&make) => Some(model),
        (Some(make), Some(model)) => Some(format!("{make} {model}")),
        (make, model) => make.or(model),
    };
    let info = ExifInfo {
        date_taken: text(Tag::DateTimeOriginal),
        gps: read_gps(exif),
        camera,
    };
    (info != ExifInfo::default()).then_some(info)
}

fn read_gps(exif: &exif::Exif) -> Option<Gps> {
    let coordinate = |tag, ref_tag, negative: u8| {
        let Value::Rational(dms) = &exif.get_field(tag, In::PRIMARY)?.value else {
            return None;
        };
        let part = |i: usize| dms.get(i).map_or(0.0, |r| r.to_f64());
        let value = part(0) + part(1) / 60.0 + part(2) / 3600.0;
        let sign = match &exif.get_field(ref_tag, In::PRIMARY)?.value {
            Value::Ascii(v) if v.first().and_then(|s| s.first()) == Some(&negative) => -1.0,
            _ => 1.0,
        };
        (!dms.is_empty() && value.is_finite()).then_some(sign * value)
    };
    let latitude = coordinate(Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S')?;
    let longitude = coordinate(Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W')?;
    let altitude = match exif
        .get_field(Tag::GPSAltitude, In::PRIMARY)
        .map(|f| &f.value)
    {
        Some(Value::Rational(v)) => v.first().map(|r| r.to_f64()).filter(|a| a.is_finite()),
        _ => None,
    }
    .map(|a| {
        let below_sea = exif
            .get_field(Tag::GPSAltitudeRef, In::PRIMARY)
            .and_then(|f| f.value.get_uint(0))
            == Some(1);
        if below_sea { -a } else { a }
    });
    Some(Gps {
        latitude,
        longitude,
        altitude,
    })
}

/// Formats `write` can put EXIF data back into.
fn writable_type(path: &Path) -> Option<FileExtension> {
    match extension(path)?.as_str() {
        "jpg" | "jpeg" | "jpe" => Some(FileExtension::JPEG),
        "tif" | "tiff" => Some(FileExtension::TIFF),
        "webp" => Some(FileExtension::WEBP),
        "heic" | "heif" => Some(FileExtension::HEIF),
        _ => None,
    }
}

pub fn is_writable(path: &Path) -> bool {
    writable_type(path).is_some()
}

/// Puts `wanted`'s date taken and GPS position back into the file, keeping
/// all its other EXIF data. The rewritten file must read back with exactly
/// those values (and the same image size) before it replaces the original.
pub fn write(path: &Path, wanted: &ExifInfo) -> Result<(), String> {
    let file_type = writable_type(path).ok_or("can't write EXIF data into this file type")?;
    let original = fs::read(path).map_err(|e| e.to_string())?;
    let had_exif = Reader::new()
        .read_from_container(&mut Cursor::new(&original))
        .is_ok();
    let mut metadata = match Metadata::new_from_vec(&original, file_type) {
        Ok(metadata) => metadata,
        Err(_) if !had_exif => Metadata::new(),
        Err(e) => return Err(format!("couldn't read its EXIF data: {e}")),
    };
    if let Some(date) = &wanted.date_taken {
        metadata.set_tag(ExifTag::DateTimeOriginal(date.clone()));
    }
    if let Some(gps) = &wanted.gps {
        set_gps(&mut metadata, gps);
    }
    let mut updated = original.clone();
    metadata
        .write_to_vec(&mut updated, file_type)
        .map_err(|e| format!("couldn't write EXIF data: {e}"))?;

    let written = Reader::new()
        .read_from_container(&mut Cursor::new(&updated))
        .ok()
        .and_then(|e| from_exif(&e))
        .unwrap_or_default();
    let date_ok = wanted.date_taken.is_none() || written.date_taken == wanted.date_taken;
    let gps_ok = match (&wanted.gps, &written.gps) {
        (None, _) => true,
        (Some(a), Some(b)) => a.same_place(b),
        (Some(_), None) => false,
    };
    if !date_ok || !gps_ok || dimensions(&original) != dimensions(&updated) {
        return Err("writing EXIF data didn't give a valid file, left unchanged".to_owned());
    }

    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = path.with_file_name(format!(".{name}.dupe-rs-tmp"));
    fs::write(&tmp, &updated)
        .and_then(|()| fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = fs::remove_file(&tmp);
            e.to_string()
        })
}

fn set_gps(metadata: &mut Metadata, gps: &Gps) {
    let dms = |degrees: f64| {
        let degrees = degrees.abs();
        let minutes = degrees.fract() * 60.0;
        let seconds = minutes.fract() * 60.0;
        vec![
            uR64 {
                nominator: degrees as u32,
                denominator: 1,
            },
            uR64 {
                nominator: minutes as u32,
                denominator: 1,
            },
            uR64 {
                nominator: (seconds * 10_000.0).round() as u32,
                denominator: 10_000,
            },
        ]
    };
    let hemisphere = |value: f64, positive: &str, negative: &str| {
        if value < 0.0 { negative } else { positive }.to_owned()
    };
    metadata.set_tag(ExifTag::GPSVersionID(vec![2, 3, 0, 0]));
    metadata.set_tag(ExifTag::GPSLatitudeRef(hemisphere(gps.latitude, "N", "S")));
    metadata.set_tag(ExifTag::GPSLatitude(dms(gps.latitude)));
    metadata.set_tag(ExifTag::GPSLongitudeRef(hemisphere(
        gps.longitude,
        "E",
        "W",
    )));
    metadata.set_tag(ExifTag::GPSLongitude(dms(gps.longitude)));
    if let Some(altitude) = gps.altitude {
        metadata.set_tag(ExifTag::GPSAltitudeRef(vec![u8::from(altitude < 0.0)]));
        metadata.set_tag(ExifTag::GPSAltitude(vec![uR64 {
            nominator: (altitude.abs() * 100.0).round() as u32,
            denominator: 100,
        }]));
    }
}

/// Width and height from the image header, if the `image` crate knows the
/// format.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A small JPEG without any EXIF data.
    pub fn write_plain_jpeg(path: &Path) {
        image::RgbImage::from_fn(32, 24, |x, y| image::Rgb([x as u8 * 8, y as u8 * 10, 90]))
            .save(path)
            .unwrap();
    }

    #[test]
    fn writes_gps_and_date_taken_and_reads_them_back() {
        let dir = tempfile::tempdir().unwrap();
        let photo = dir.path().join("photo.jpg");
        write_plain_jpeg(&photo);
        assert_eq!(read(&photo), None);

        let wanted = ExifInfo {
            date_taken: Some("2019:07:14 18:03:22".to_owned()),
            gps: Some(Gps {
                latitude: 47.80949,
                longitude: -13.05501,
                altitude: Some(424.5),
            }),
            camera: None,
        };
        write(&photo, &wanted).unwrap();

        let got = read(&photo).unwrap();
        assert_eq!(got.date_taken, wanted.date_taken);
        let gps = got.gps.unwrap();
        assert!(gps.same_place(&wanted.gps.unwrap()), "{gps:?}");
        assert!((gps.altitude.unwrap() - 424.5).abs() < 0.01);
        assert_eq!(image::open(&photo).unwrap().width(), 32);
        assert!(!dir.path().join(".photo.jpg.dupe-rs-tmp").exists());
    }

    #[test]
    fn refuses_formats_it_cannot_write() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clip.mp4");
        fs::write(&file, b"not a photo").unwrap();
        assert!(write(&file, &ExifInfo::default()).is_err());
        assert_eq!(fs::read(&file).unwrap(), b"not a photo");
    }
}
