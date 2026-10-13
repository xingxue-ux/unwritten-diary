library;

import 'dart:io';
import 'package:crypto/crypto.dart';

class CopyCancelled implements Exception {}

class CopyTooLarge implements Exception {
  CopyTooLarge(this.limit);
  final int limit;
}

class CopyCancellation {
  bool _cancelled = false;
  void cancel() => _cancelled = true;
  void check() {
    if (_cancelled) throw CopyCancelled();
  }
}

class StagedCopy {
  const StagedCopy(this.byteSize, this.sha256);
  final int byteSize;
  final String sha256;
}

class _DigestSink implements Sink<Digest> {
  Digest? value;
  @override
  void add(Digest data) => value = data;
  @override
  void close() {}
}

/// The destination must be the core-issued staging ticket, never a display name.
/// Awaiting every write bounds buffering. An interrupted file remains staged for
/// core recovery; it is never reported as an accepted asset.
Future<StagedCopy> copyToStaging({
  required Stream<List<int>> source,
  required File destination,
  required CopyCancellation cancellation,
  int? maxBytes,
  void Function(int copiedBytes)? onProgress,
}) async {
  cancellation.check();
  if (maxBytes != null && maxBytes < 0) {
    throw ArgumentError.value(maxBytes, 'maxBytes');
  }
  final output = await destination.open(mode: FileMode.write);
  final digest = _DigestSink();
  final hashing = sha256.startChunkedConversion(digest);
  var copied = 0;
  try {
    await for (final chunk in source) {
      cancellation.check();
      if (maxBytes != null && chunk.length > maxBytes - copied) {
        throw CopyTooLarge(maxBytes);
      }
      await output.writeFrom(chunk);
      hashing.add(chunk);
      copied += chunk.length;
      onProgress?.call(copied);
    }
    cancellation.check();
    await output.flush();
    hashing.close();
    return StagedCopy(copied, digest.value.toString());
  } finally {
    await output.close();
  }
}
