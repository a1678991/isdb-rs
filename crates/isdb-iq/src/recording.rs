//! An IQ data file with its sample format, rate and centre frequency.

use crate::sigmf::Meta;
use crate::{C32, Error, Result, SampleFormat};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use tracing::debug;

/// The SigMF metadata path of data file `data`: `NAME.sigmf-data` →
/// `NAME.sigmf-meta`, any other name → the name with `.sigmf-meta`
/// appended (`bs1.cs16` → `bs1.cs16.sigmf-meta`).
pub fn meta_path_for(data: &Path) -> PathBuf {
    let s = data.as_os_str().to_string_lossy();
    match s.strip_suffix(".sigmf-data") {
        Some(base) => PathBuf::from(format!("{base}.sigmf-meta")),
        None => PathBuf::from(format!("{s}.sigmf-meta")),
    }
}

/// The data path of a recording given by its data file, its
/// `.sigmf-meta` file or the SigMF base name (`NAME` for
/// `NAME.sigmf-data`).
pub fn data_path_for(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if let Some(base) = s.strip_suffix(".sigmf-meta") {
        let sigmf = PathBuf::from(format!("{base}.sigmf-data"));
        // `bs1.cs16.sigmf-meta` describes `bs1.cs16`.
        return if sigmf.exists() {
            sigmf
        } else {
            PathBuf::from(base)
        };
    }
    if !path.exists() {
        let sigmf = PathBuf::from(format!("{s}.sigmf-data"));
        if sigmf.exists() {
            return sigmf;
        }
    }
    path.to_path_buf()
}

/// An IQ recording on disk.
#[derive(Debug)]
pub struct Recording {
    /// The data file.
    pub data_path: PathBuf,
    /// The SigMF metadata, if the recording has a `.sigmf-meta` file.
    pub meta: Option<Meta>,
    /// Sample format.
    pub format: SampleFormat,
    /// Sample rate in Hz.
    pub sample_rate: f64,
    /// Centre frequency in Hz of the first segment, if known.
    pub frequency: Option<f64>,
    /// Number of whole samples in the data file.
    pub samples: u64,
    file: std::fs::File,
}

impl Recording {
    /// Open the recording at `path` (data file, `.sigmf-meta` file or SigMF
    /// base name). Its SigMF metadata gives the format and rate; `format`
    /// and `sample_rate` override them and are needed when there is no
    /// metadata.
    pub fn open(
        path: &Path,
        format: Option<SampleFormat>,
        sample_rate: Option<f64>,
    ) -> Result<Self> {
        let data_path = data_path_for(path);
        let io = |p: &Path| {
            let path = p.display().to_string();
            move |source| Error::Io { path, source }
        };
        let meta_path = meta_path_for(&data_path);
        let meta = if meta_path.exists() {
            let text = std::fs::read_to_string(&meta_path).map_err(io(&meta_path))?;
            let m = Meta::from_json(&text).map_err(|e| Error::Meta {
                path: meta_path.display().to_string(),
                source: Box::new(e),
            })?;
            debug!(
                path = %meta_path.display(),
                format = %m.format,
                sample_rate_hz = m.sample_rate,
                segments = m.captures.len(),
                hw = m.hw.as_deref().unwrap_or(""),
                "SigMF metadata {} read: {}, {} Hz, {} capture segment(s){}",
                meta_path.display(),
                m.format,
                m.sample_rate,
                m.captures.len(),
                m.hw.as_deref().map_or(String::new(), |h| format!(", hardware {h}"))
            );
            Some(m)
        } else {
            debug!(
                path = %meta_path.display(),
                "no SigMF metadata at {}",
                meta_path.display()
            );
            None
        };
        let no_meta = |missing| Error::NoMeta {
            data_path: data_path.display().to_string(),
            meta_path: meta_path.display().to_string(),
            missing,
        };
        let format = format
            .or(meta.as_ref().map(|m| m.format))
            .ok_or_else(|| no_meta("sample format"))?;
        let sample_rate = sample_rate
            .or(meta.as_ref().map(|m| m.sample_rate))
            .ok_or_else(|| no_meta("sample rate"))?;
        let frequency = meta
            .as_ref()
            .and_then(|m| m.captures.first())
            .and_then(|c| c.frequency);
        let file = std::fs::File::open(&data_path).map_err(io(&data_path))?;
        let len = file.metadata().map_err(io(&data_path))?.len();
        let samples = len / format.bytes_per_sample() as u64;
        debug!(
            path = %data_path.display(),
            format = %format,
            sample_rate_hz = sample_rate,
            frequency_hz = frequency.unwrap_or(f64::NAN),
            samples,
            "recording {} opened: {samples} samples of {format} at {sample_rate} Hz{}",
            data_path.display(),
            frequency.map_or(String::new(), |f| format!(", centre {f} Hz"))
        );
        Ok(Self {
            data_path,
            meta,
            format,
            sample_rate,
            frequency,
            samples,
            file,
        })
    }

    /// Read up to `n` samples from sample index `start` (fewer at the end
    /// of the file).
    pub fn read(&mut self, start: u64, n: usize) -> Result<Vec<C32>> {
        let bps = self.format.bytes_per_sample();
        let n = n.min(usize::try_from(self.samples.saturating_sub(start)).unwrap_or(usize::MAX));
        let mut bytes = vec![0u8; n * bps];
        let io = |source| Error::Io {
            path: self.data_path.display().to_string(),
            source,
        };
        self.file
            .seek(SeekFrom::Start(start * bps as u64))
            .map_err(io)?;
        self.file.read_exact(&mut bytes).map_err(io)?;
        let mut out = Vec::with_capacity(n);
        self.format.decode(&bytes, &mut out);
        Ok(out)
    }

    /// The contiguous segments (sample ranges) of the recording: from the
    /// SigMF capture segments, or the whole file.
    pub fn segments(&self) -> Vec<std::ops::Range<u64>> {
        match &self.meta {
            Some(m) => m.segment_ranges(self.samples),
            None => std::iter::once(0..self.samples).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_paths() {
        assert_eq!(
            meta_path_for(Path::new("/x/bs1.sigmf-data")),
            PathBuf::from("/x/bs1.sigmf-meta")
        );
        assert_eq!(
            meta_path_for(Path::new("bs1.cs16")),
            PathBuf::from("bs1.cs16.sigmf-meta")
        );
    }

    #[test]
    fn open_with_and_without_metadata() {
        let dir = std::env::temp_dir().join(format!("isdb-iq-rec-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let data = dir.join("r.sigmf-data");
        // Three cs16/2048 samples and one stray byte.
        let mut b = Vec::new();
        for v in [1024i16, -1024, 2048, 0, 0, -2048] {
            b.extend(v.to_le_bytes());
        }
        b.push(7);
        std::fs::write(&data, &b).unwrap();
        // No metadata: format and rate are required.
        assert!(matches!(
            Recording::open(&data, None, Some(10.0)),
            Err(Error::NoMeta {
                missing: "sample format",
                ..
            })
        ));
        assert!(matches!(
            Recording::open(&data, Some(SampleFormat::Cs8), None),
            Err(Error::NoMeta {
                missing: "sample rate",
                ..
            })
        ));
        let mut r = Recording::open(&data, Some(SampleFormat::AD936X_CS16), Some(10.0)).unwrap();
        assert_eq!(r.samples, 3);
        assert_eq!(
            r.read(1, 10).unwrap(),
            vec![C32::new(1.0, 0.0), C32::new(0.0, -1.0)]
        );
        // With metadata, opened by the base name and by the meta file.
        let mut m = Meta::new(SampleFormat::AD936X_CS16, 57.72e6);
        m.captures.push(crate::sigmf::Segment {
            sample_start: 0,
            frequency: Some(1.04948e9),
            ..Default::default()
        });
        m.captures.push(crate::sigmf::Segment {
            sample_start: 2,
            frequency: Some(1.04948e9),
            ..Default::default()
        });
        std::fs::write(dir.join("r.sigmf-meta"), m.to_json()).unwrap();
        for p in ["r", "r.sigmf-meta", "r.sigmf-data"] {
            let mut r = Recording::open(&dir.join(p), None, None).unwrap();
            assert_eq!((r.sample_rate, r.frequency), (57.72e6, Some(1.04948e9)));
            assert_eq!(r.segments(), vec![0..2, 2..3]);
            assert_eq!(r.read(0, 1).unwrap(), vec![C32::new(0.5, -0.5)]);
        }
        // Invalid metadata: the file and the reason.
        std::fs::write(dir.join("r.sigmf-meta"), "{}").unwrap();
        let e = Recording::open(&data, None, None).unwrap_err();
        assert!(
            matches!(&e, Error::Meta { source, .. }
                if matches!(**source, Error::MissingField { field: "global object" })),
            "{e:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
