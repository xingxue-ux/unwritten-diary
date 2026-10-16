//! 录音音频的解码：**只做 PCM WAV**，按窗口随机读，不把整段音频读进内存。
//!
//! # 为什么只做 WAV
//!
//! 录音链（`recordings.rs`）最终化的逻辑音频是**平台层写下的片段按序拼接**，
//! 上游约定的是 PCM WAV（见 `docs/architecture/m3-转写链.md` 的「音频容器」一节）。
//! 导入进来的 mp3/m4a/视频音轨是另一条路：**这一片不解码它们**——转写链只承诺
//! 「自己录的音」能转写。硬塞一个半吊子解码器会让「不支持的编码」变成一段乱码
//! 文本，那比明确报不支持坏得多。
//!
//! # 为什么按窗口读
//!
//! 一小时 16 kHz 单声道 f32 展开是 230 MiB，移动端不能这么用内存。
//! WAV 的 PCM 数据是可定位的，于是每个窗口只读它自己那几十秒：
//! `WavReader::read_window` 返回单声道 f32（多声道在这里混好，重采样交给引擎）。
//!
//! # 拼接出来的音频有一个坑：它可能不是**合法**的 WAV
//!
//! `recordings::finalize` 是把片段文件的字节直接拼起来的。若每个片段各自带 44 字节
//! 头，拼出来就是「头 + 数据 + 头 + 数据」——第二个头会被当成 PCM 采样读进来，
//! 听感上是几毫秒的爆音，转写结果里可能出现一两个莫名其妙的字。这一片**不修**它
//! （修它要改录音最终化的口径，属于 F1 录音链），但如实记在文档的「没有做到的」里。
//! 解码器对这种情况的行为是确定的：只认第一个头，后面的字节按采样读。

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use crate::error::{CoreError, Result};

/// WAV 里我们认识的采样格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    /// 16 位有符号整数（PCM，`audio_format = 1`）。录音链的默认形态。
    Pcm16,
    /// 32 位浮点（`audio_format = 3`，IEEE float）。
    Float32,
}

/// 一个 PCM WAV 的格式与数据区。
#[derive(Debug)]
pub struct WavReader {
    file: File,
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    /// 数据区在文件里的起始偏移。
    data_offset: u64,
    /// 数据区的字节数。
    data_bytes: u64,
    /// 每个采样帧的字节数（`channels × 位宽/8`）。
    frame_bytes: u64,
}

/// 解析出来的 WAV 布局（文件与内存两条路共用）。
#[derive(Debug, Clone, Copy)]
struct WavLayout {
    sample_rate: u32,
    channels: u16,
    format: SampleFormat,
    data_offset: u64,
    data_bytes: u64,
    frame_bytes: u64,
}

/// 从「能按偏移读字节」的来源解析 WAV 布局。
///
/// 文件与内存两条路共用它：文件那条按偏移 seek（不把整段音频读进内存），
/// 内存那条对着 `&[u8]` 读（质量集的占位音频、探针音频都是内存里的 WAV）。
fn parse_layout<F>(mut read_at: F, total_len: u64, label: &str) -> Result<WavLayout>
where
    F: FnMut(u64, &mut [u8]) -> std::io::Result<()>,
{
    let mut header = vec![0_u8; 12];
    read_at(0, &mut header).map_err(|error| read_failed(error, label))?;
    if &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(CoreError::ExtractionFailed {
            reason: format!(
                "{label} 不是 RIFF/WAVE 容器（这一片只解码 PCM WAV；导入的压缩音频还不支持）"
            ),
        });
    }

    let mut position = 12_u64;
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut data: Option<(u64, u64)> = None;
    // 顺序扫块，允许 `fmt ` 与 `data` 之间有别的块（LIST/fact…）。
    loop {
        let mut chunk_header = vec![0_u8; 8];
        if read_at(position, &mut chunk_header).is_err() {
            break;
        }
        let id = &chunk_header[0..4];
        let size = u32::from_le_bytes([
            chunk_header[4],
            chunk_header[5],
            chunk_header[6],
            chunk_header[7],
        ]) as u64;
        let body = position + 8;

        if id == b"fmt " {
            let mut fmt = vec![0_u8; size.min(64) as usize];
            read_at(body, &mut fmt).map_err(|error| read_failed(error, label))?;
            if fmt.len() < 16 {
                return Err(CoreError::ExtractionFailed {
                    reason: format!("{label} 的 fmt 块只有 {} 字节，读不出格式", fmt.len()),
                });
            }
            let audio_format = u16::from_le_bytes([fmt[0], fmt[1]]);
            let channels = u16::from_le_bytes([fmt[2], fmt[3]]);
            let sample_rate = u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]);
            let bits = u16::from_le_bytes([fmt[14], fmt[15]]);
            format = Some((audio_format, channels, sample_rate, bits));
        } else if id == b"data" {
            let available = total_len.saturating_sub(body);
            data = Some((body, size.min(available)));
        }

        // 块按偶数字节对齐。
        position = body + size + (size % 2);
        if data.is_some() && format.is_some() {
            break;
        }
        if position + 8 > total_len {
            break;
        }
    }

    let Some((audio_format, channels, sample_rate, bits)) = format else {
        return Err(CoreError::ExtractionFailed {
            reason: format!("{label} 里没有 fmt 块，读不出采样格式"),
        });
    };
    let Some((data_offset, data_bytes)) = data else {
        return Err(CoreError::ExtractionFailed {
            reason: format!("{label} 里没有 data 块，没有可读的音频"),
        });
    };
    if channels == 0 || sample_rate == 0 {
        return Err(CoreError::ExtractionFailed {
            reason: format!("{label} 的声道数或采样率为 0，不是可解码的音频"),
        });
    }
    let sample_format = match (audio_format, bits) {
        (1, 16) => SampleFormat::Pcm16,
        (3, 32) => SampleFormat::Float32,
        (1, 8) | (1, 24) | (1, 32) => {
            return Err(CoreError::ExtractionFailed {
                reason: format!(
                    "{label} 是 {bits} 位整数 PCM；这一片只解码 16 位整数与 32 位浮点 WAV"
                ),
            })
        }
        (other, bits) => {
            return Err(CoreError::ExtractionFailed {
                reason: format!(
                    "{label} 的音频格式是 {other}（{bits} 位）；这一片只解码未压缩 PCM WAV"
                ),
            })
        }
    };
    let frame_bytes = u64::from(channels) * bytes_per_sample(sample_format);
    Ok(WavLayout {
        sample_rate,
        channels,
        format: sample_format,
        data_offset,
        data_bytes,
        frame_bytes,
    })
}

/// 把一段 PCM 字节转成单声道 f32（多声道按算术平均混音）。
fn to_mono(raw: &[u8], channels: usize, format: SampleFormat) -> Vec<f32> {
    let mut mono = Vec::with_capacity(raw.len() / (channels * bytes_per_sample(format) as usize));
    match format {
        SampleFormat::Pcm16 => {
            for frame in raw.chunks_exact(2 * channels) {
                let mut sum = 0_f32;
                for channel in 0..channels {
                    let value = i16::from_le_bytes([frame[channel * 2], frame[channel * 2 + 1]]);
                    sum += f32::from(value) / 32768.0;
                }
                mono.push(sum / channels as f32);
            }
        }
        SampleFormat::Float32 => {
            for frame in raw.chunks_exact(4 * channels) {
                let mut sum = 0_f32;
                for channel in 0..channels {
                    let bytes = [
                        frame[channel * 4],
                        frame[channel * 4 + 1],
                        frame[channel * 4 + 2],
                        frame[channel * 4 + 3],
                    ];
                    sum += f32::from_le_bytes(bytes);
                }
                mono.push(sum / channels as f32);
            }
        }
    }
    mono
}

fn read_failed(error: std::io::Error, label: &str) -> CoreError {
    CoreError::ExtractionFailed {
        reason: format!("读 {label} 失败：{error}"),
    }
}

/// 内存里的一段 WAV 解码结果。
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedPcm {
    /// 单声道 f32（-1..1）。
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    /// 原始声道数（混音前）。
    pub channels: u16,
    pub duration_ms: i64,
}

/// 解码内存里的 WAV 字节（质量集的占位音频、探针音频、测试用）。
///
/// 与文件那条路共用同一套布局解析与混音规则，所以两条路的判据一致。
pub fn decode_bytes(bytes: &[u8], label: &str) -> Result<DecodedPcm> {
    let layout = parse_layout(
        |offset, buffer| {
            let start = offset as usize;
            let end = start.saturating_add(buffer.len());
            if end > bytes.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "越过了音频字节的末尾",
                ));
            }
            buffer.copy_from_slice(&bytes[start..end]);
            Ok(())
        },
        bytes.len() as u64,
        label,
    )?;
    let start = layout.data_offset as usize;
    let end = start.saturating_add(layout.data_bytes as usize).min(bytes.len());
    let raw = bytes.get(start..end).unwrap_or_default();
    let samples = to_mono(raw, usize::from(layout.channels), layout.format);
    let frames = samples.len() as u128;
    let duration_ms = (frames * 1000 / u128::from(layout.sample_rate.max(1)))
        .min(i64::MAX as u128) as i64;
    Ok(DecodedPcm {
        samples,
        sample_rate: layout.sample_rate,
        channels: layout.channels,
        duration_ms,
    })
}

impl WavReader {
    /// 打开一个 WAV 并解析头部。不认识的形式报 `unsupported_format`，理由写清
    /// 「这个文件是什么、为什么读不了」，绝不返回一段乱码。
    pub fn open(path: &Path) -> Result<Self> {
        let mut file = File::open(path).map_err(|error| CoreError::AssetMissing {
            asset_id: path.to_string_lossy().into_owned(),
            reason: format!("音频文件打不开：{error}"),
        })?;
        let total_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        let label = path.display().to_string();
        let layout = parse_layout(
            |offset, buffer| read_exact_at(&mut file, buffer, offset),
            total_len,
            &label,
        )?;
        Ok(Self {
            file,
            sample_rate: layout.sample_rate,
            channels: layout.channels,
            format: layout.format,
            data_offset: layout.data_offset,
            data_bytes: layout.data_bytes,
            frame_bytes: layout.frame_bytes,
        })
    }


    /// 采样帧数（每声道）。
    pub fn frames(&self) -> u64 {
        self.data_bytes / self.frame_bytes
    }

    /// 音频时长（毫秒）。
    pub fn duration_ms(&self) -> i64 {
        let frames = self.frames() as u128;
        let ms = frames * 1000 / u128::from(self.sample_rate.max(1));
        ms.min(i64::MAX as u128) as i64
    }

    /// 读 `[start_frame, start_frame + count_frames)`，返回**单声道** f32
    /// （多声道按算术平均混音）。越界部分如实截短，不补零、不重复。
    pub fn read_window(&mut self, start_frame: u64, count_frames: u64) -> Result<Vec<f32>> {
        let available = self.frames().saturating_sub(start_frame);
        let frames = count_frames.min(available);
        if frames == 0 {
            return Ok(Vec::new());
        }
        let offset = self
            .data_offset
            .saturating_add(start_frame.saturating_mul(self.frame_bytes));
        let bytes = frames.saturating_mul(self.frame_bytes);
        let mut raw = vec![0_u8; bytes.min(usize::MAX as u64) as usize];
        read_exact_at(&mut self.file, &mut raw, offset).map_err(|error| CoreError::ExtractionFailed {
            reason: format!("读音频窗口失败（数据区偏移 {offset}）：{error}"),
        })?;

        Ok(to_mono(
            &raw,
            usize::from(self.channels),
            self.format,
        ))
    }

    /// 全部解码成单声道 f32。**只给测试与小文件用**：生产路径按窗口读，
    /// 不把整段音频放进内存。
    #[cfg(test)]
    pub fn read_all(&mut self) -> Result<Vec<f32>> {
        let frames = self.frames();
        self.read_window(0, frames)
    }
}

fn bytes_per_sample(format: SampleFormat) -> u64 {
    match format {
        SampleFormat::Pcm16 => 2,
        SampleFormat::Float32 => 4,
    }
}

fn read_exact_at(file: &mut File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;
    use std::io::Write;

    /// 造一个合法的 16 位 PCM WAV。
    fn write_wav(path: &Path, sample_rate: u32, channels: u16, samples: &[i16]) {
        let data_bytes = (samples.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        let byte_rate = sample_rate * u32::from(channels) * 2;
        out.extend_from_slice(&byte_rate.to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_bytes.to_le_bytes());
        for sample in samples {
            out.extend_from_slice(&sample.to_le_bytes());
        }
        let mut file = File::create(path).unwrap();
        file.write_all(&out).unwrap();
    }

    #[test]
    fn reads_pcm16_mono_and_reports_duration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mono.wav");
        let samples: Vec<i16> = (0..1600).map(|index| (index % 100) as i16).collect();
        write_wav(&path, 16_000, 1, &samples);

        let mut reader = WavReader::open(&path).unwrap();
        assert_eq!(reader.sample_rate, 16_000);
        assert_eq!(reader.channels, 1);
        assert_eq!(reader.frames(), 1600);
        assert_eq!(reader.duration_ms(), 100);
        let all = reader.read_all().unwrap();
        assert_eq!(all.len(), 1600);
        assert!((all[1] - 1.0 / 32768.0).abs() < 1e-9);
    }

    #[test]
    fn downmixes_stereo_and_reads_windows_by_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stereo.wav");
        // 左右声道分别是 +1000 / -1000：混音后应当抵消成 0。
        let mut samples = Vec::new();
        for _ in 0..800 {
            samples.push(1000_i16);
            samples.push(-1000_i16);
        }
        write_wav(&path, 8_000, 2, &samples);

        let mut reader = WavReader::open(&path).unwrap();
        assert_eq!(reader.frames(), 800);
        let window = reader.read_window(100, 200).unwrap();
        assert_eq!(window.len(), 200);
        assert!(window.iter().all(|value| value.abs() < 1e-9));

        // 越界窗口如实截短。
        let tail = reader.read_window(700, 900).unwrap();
        assert_eq!(tail.len(), 100);
        assert!(reader.read_window(900, 100).unwrap().is_empty());
    }

    #[test]
    fn rejects_non_wav_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, "这不是音频".as_bytes()).unwrap();
        let error = WavReader::open(&path).unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnsupportedFormat);
        assert!(error.to_string().contains("RIFF/WAVE"), "{error}");
    }

    #[test]
    fn rejects_compressed_format_honestly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("adpcm.wav");
        // 合法的 RIFF/WAVE 头，但 audio_format = 2（ADPCM）。
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&36_u32.to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16_u32.to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&1_u16.to_le_bytes());
        out.extend_from_slice(&16_000_u32.to_le_bytes());
        out.extend_from_slice(&32_000_u32.to_le_bytes());
        out.extend_from_slice(&2_u16.to_le_bytes());
        out.extend_from_slice(&4_u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&0_u32.to_le_bytes());
        std::fs::write(&path, out).unwrap();

        let error = WavReader::open(&path).unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnsupportedFormat);
        assert!(error.to_string().contains("PCM WAV"), "{error}");
    }
}