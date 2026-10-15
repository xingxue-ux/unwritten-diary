library;

import 'dart:io';
import 'dart:async';
import 'package:diary_api/diary_api.dart';
import 'package:crypto/crypto.dart';

class CopyCancelled implements Exception {}

class CopyTooLarge implements Exception {
  CopyTooLarge(this.limit);
  final int limit;
}

class CopyCancellation {
  bool _cancelled = false;
  final Completer<void> _whenCancelled = Completer<void>();
  void cancel() {
    if (_cancelled) return;
    _cancelled = true;
    _whenCancelled.complete();
  }

  void check() {
    if (_cancelled) throw CopyCancelled();
  }
}

class StagedCopy {
  const StagedCopy(this.byteSize, this.sha256);
  final int byteSize;
  final String sha256;
}

/// An interrupted operation retains its identity for recovery/status queries.
class ImportInterrupted implements Exception {
  ImportInterrupted(this.importId, this.cause);
  final String importId;
  final Object cause;
}

/// Owned by the application service, not by a capture page. Keep the same
/// operationId when retrying a preparation whose response was interrupted.
class FileImportCoordinator {
  FileImportCoordinator(this.api);
  final DiaryApi api;
  final Set<String> _active = {};

  Future<ImportStatus> importFile({
    required String captureId,
    required String displayName,
    required String operationId,
    required Stream<List<int>> Function() openRead,
    required CopyCancellation cancellation,
    String? mimeHint,
    int? sizeHint,
    void Function(int copiedBytes)? onProgress,
    Future<void> Function(ImportTicket ticket)? onPrepared,
    void Function()? onCopyComplete,
  }) async {
    if (!_active.add(operationId)) {
      throw StateError('This import operation is already running.');
    }
    ImportTicket? ticket;
    try {
      cancellation.check();
      ticket = await api.prepareImport(
        captureId: captureId,
        displayName: displayName,
        mimeHint: mimeHint,
        sizeHint: sizeHint,
        operationId: operationId,
      );
      if (onPrepared != null) await onPrepared(ticket);
      cancellation.check();
      final existing = await api.importStatus(ticket.importId);
      // An idempotent retry must never truncate an already accepted asset.
      if (existing.state == ImportState.ready ||
          existing.state == ImportState.verifying) {
        return existing;
      }
      if (existing.state == ImportState.error) {
        return existing;
      }
      final copy = await copyToStaging(
        source: openRead(),
        destination: File(ticket.stagingTicket),
        cancellation: cancellation,
        maxBytes: ticket.maxBytes,
        onProgress: onProgress,
      );
      cancellation.check();
      onCopyComplete?.call();
      cancellation.check();
      // The core detects the real MIME and verifies bytes/hash; a picker hint
      // is not evidence of a valid file. Only its ready response means accepted.
      return await api.finishImport(
        importId: ticket.importId,
        ticket: ticket.stagingTicket,
        manifest: ImportManifest(
          copiedBytes: copy.byteSize,
          sha256: copy.sha256,
          detectedMime: mimeHint ?? 'application/octet-stream',
          originalName: displayName,
        ),
      );
    } catch (error) {
      if (ticket != null) throw ImportInterrupted(ticket.importId, error);
      rethrow;
    } finally {
      _active.remove(operationId);
    }
  }
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
  final input = StreamIterator<List<int>>(source);
  var copied = 0;
  try {
    while (true) {
      final hasNext = await Future.any([
        input.moveNext(),
        cancellation._whenCancelled.future.then((_) => false),
      ]);
      cancellation.check();
      if (!hasNext) break;
      final chunk = input.current;
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
    cancellation.check();
    hashing.close();
    return StagedCopy(copied, digest.value.toString());
  } finally {
    try {
      await input.cancel();
    } finally {
      await output.close();
    }
  }
}
