import 'dart:io';
import 'dart:math' as math;
import 'dart:typed_data';

/// 一条音频的波形摘要：每个桶一个 0..1 的峰值。
///
/// 不把整段音频读进内存：录音可能是一小时，16 kHz 单声道 16 位就是 115 MiB。
/// 这里按块流式扫一遍，只留下 [buckets] 个峰值。
class Waveform {
  const Waveform({required this.peaks, required this.duration});

  final List<double> peaks;
  final Duration duration;

  bool get isEmpty => peaks.isEmpty;
}

/// 读 PCM WAV 的波形。不是可识别的 WAV（或读不动）时返回 `null`——
/// 波形只是显示，读不出来不该让播放失败。
Future<Waveform?> loadWaveform(String path, {int buckets = 160}) async {
  RandomAccessFile? file;
  try {
    file = await File(path).open();
    final length = await file.length();
    if (length < 44) return null;
    final header = await file.read(12);
    if (header.length < 12 ||
        String.fromCharCodes(header.sublist(0, 4)) != 'RIFF' ||
        String.fromCharCodes(header.sublist(8, 12)) != 'WAVE') {
      return null;
    }

    var position = 12;
    int? channels;
    int? sampleRate;
    int? bits;
    int? dataOffset;
    int? dataBytes;
    while (position + 8 <= length) {
      await file.setPosition(position);
      final chunk = await file.read(8);
      if (chunk.length < 8) break;
      final id = String.fromCharCodes(chunk.sublist(0, 4));
      final size = ByteData.sublistView(chunk, 4, 8).getUint32(0, Endian.little);
      final body = position + 8;
      if (id == 'fmt ' && size >= 16) {
        await file.setPosition(body);
        final fmt = await file.read(math.min(size, 64));
        if (fmt.length >= 16) {
          channels = ByteData.sublistView(fmt, 2, 4).getUint16(0, Endian.little);
          sampleRate = ByteData.sublistView(fmt, 4, 8).getUint32(0, Endian.little);
          bits = ByteData.sublistView(fmt, 14, 16).getUint16(0, Endian.little);
        }
      } else if (id == 'data') {
        dataOffset = body;
        dataBytes = math.min(size, math.max(0, length - body));
      }
      position = body + size + (size.isOdd ? 1 : 0);
      if (dataOffset != null && channels != null) break;
    }

    if (channels == null ||
        channels <= 0 ||
        sampleRate == null ||
        sampleRate <= 0 ||
        bits != 16 ||
        dataOffset == null ||
        dataBytes == null ||
        dataBytes <= 0) {
      return null;
    }

    final frameBytes = channels * 2;
    final frames = dataBytes ~/ frameBytes;
    if (frames <= 0) return null;
    final bucketFrames = math.max(1, (frames / buckets).ceil());
    final peaks = <double>[];
    var bucketPeak = 0.0;
    var framesInBucket = 0;

    const chunkBytes = 1 << 18; // 256 KiB
    var consumed = 0;
    await file.setPosition(dataOffset);
    while (consumed < dataBytes) {
      final want = math.min(chunkBytes, dataBytes - consumed);
      final chunk = await file.read(want);
      if (chunk.isEmpty) break;
      consumed += chunk.length;
      final view = ByteData.sublistView(Uint8List.fromList(chunk));
      final available = chunk.length ~/ frameBytes;
      for (var frame = 0; frame < available; frame++) {
        var sample = 0;
        for (var channel = 0; channel < channels; channel++) {
          final value = view.getInt16(
            (frame * channels + channel) * 2,
            Endian.little,
          );
          if (value.abs() > sample) sample = value.abs();
        }
        final normalized = sample / 32768.0;
        if (normalized > bucketPeak) bucketPeak = normalized;
        framesInBucket++;
        if (framesInBucket >= bucketFrames) {
          peaks.add(bucketPeak);
          bucketPeak = 0;
          framesInBucket = 0;
        }
      }
    }
    if (framesInBucket > 0) peaks.add(bucketPeak);
    // 归一化：整段都很轻的时候，波形也要看得出起伏。
    final loudest = peaks.fold<double>(0, math.max);
    final scaled = loudest <= 0.0001
        ? List<double>.filled(peaks.length, 0)
        : peaks.map((peak) => (peak / loudest).clamp(0.0, 1.0)).toList();
    return Waveform(
      peaks: scaled,
      duration: Duration(
        milliseconds: (frames * 1000 / sampleRate).round(),
      ),
    );
  } on Object {
    return null;
  } finally {
    await file?.close();
  }
}
